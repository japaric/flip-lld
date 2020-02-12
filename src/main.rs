#![deny(warnings)]

use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, Error};
use goblin::elf::Elf;
use walkdir::WalkDir;

fn main() -> Result<(), Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let rust_lld = rust_lld()?;

    // run the linker exactly as `rustc` instructed
    if !Command::new(&rust_lld).args(&args).status()?.success() {
        return Err(anyhow!("first `rust-lld` invocation failed"));
    }

    // retrieve the output file name
    let output = &get_o_value(&args)?.to_owned();
    let Boundaries {
        mut merged_sections,
        stack_top,
        address,
        size,
    } = get_boundaries(&output)?;

    let mut ok = stack_top <= address + size;
    let mut new_boundary = stack_top - size;
    // TODO we may want to upper bound the number of iterations this loop does?
    'link: while !ok {
        // 8-byte align the new boundary; most architectures need the stack to
        // be 4-byte or 8-byte aligned
        let rem = new_boundary % 8;
        if rem != 0 {
            new_boundary -= rem;
        }

        // swap the location of the stack and the statically allocated data so
        // they can't run into each other
        let mut new_args = args.clone();
        new_args.insert(2, format!("--defsym=__ram_start__={}", new_boundary));
        new_args.insert(2, format!("--defsym=__stack_top__={}", new_boundary));

        let mut c = Command::new(&rust_lld);
        c.args(&new_args);
        if !c.status()?.success() {
            return Err(anyhow!("second `rust-lld` invocation failed"));
        }

        // now do a sanity check
        let bytes = &fs::read(output)?;
        let elf = Elf::parse(bytes)?;
        let mut fatal_error = false;
        ok = true;

        // all sections must
        // - start at an address higher than `new_boundary`; this ensures the
        // stack won't collide into them
        // - end at an address lower or equal to the initial `stack_top`, as
        // this is likely the boundary of the RAM region
        for sh in elf.section_headers {
            if let Some(name) = elf.shdr_strtab.get(sh.sh_name).and_then(|res| res.ok()) {
                if merged_sections.remove(name) {
                    let start = sh.sh_addr;
                    let end = start + sh.sh_size;

                    if start < new_boundary {
                        // didn't properly merge sections in `get_boundaries`
                        fatal_error = true;
                        break;
                    }

                    if end > stack_top {
                        // internal alignment requirements pushed the sections
                        // past the RAM boundary
                        ok = false;

                        // we need to shift the boundary lower
                        new_boundary -= end - stack_top;
                        continue 'link;
                    }
                }
            }
        }

        // a section disappeared in the second linker invocation
        fatal_error |= !merged_sections.is_empty();

        if fatal_error {
            // remove the linked binary because it is invalid
            let _ = fs::remove_file(output);

            // unexpected error
            return Err(anyhow!(
                "We are sorry.\
We couldn't make the linker do as we intended.\
Please file a bug report with steps to build this program so we can do better."
            ));
        }
    }

    Ok(())
}

/// finds the path to rust-lld
fn rust_lld() -> Result<PathBuf, Error> {
    let sysroot = PathBuf::from(
        String::from_utf8(
            Command::new("rustc")
                .args(&["--print", "sysroot"])
                .output()?
                .stdout,
        )?
        .trim(),
    );

    for e in WalkDir::new(&sysroot) {
        let e = e?;
        let file_name = e.file_name();

        if file_name == "rust-lld" || file_name == "rust-lld.exe" {
            return Ok(e.into_path());
        }
    }

    Err(anyhow!("`rust-lld` was not found"))
}

/// returns the value of the `-o` flag
fn get_o_value(args: &[String]) -> Result<&Path, Error> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "-o" {
            if let Some(path) = args.next() {
                return Ok(Path::new(path));
            }
        }
    }

    Err(anyhow!(
        "no `-o` flag was passed to the first `rust-lld` invocation"
    ))
}

/// Boundaries of the statically allocated memory
struct Boundaries {
    merged_sections: HashSet<String>,
    address: u64,
    size: u64,
    // the originally chosen top of the stack
    stack_top: u64,
}

// analyze the linker sections of the ELF file and extract the boundaries of statically allocated
// memory
//
// this assumes that either a .bss or a .data section exists in the ELF
fn get_boundaries(path: &Path) -> Result<Boundaries, Error> {
    let bytes = &fs::read(path)?;
    let mut elf = Elf::parse(bytes)?;

    // sections that will be allocated in device memory
    elf.section_headers.sort_by_key(|sect| sect.sh_addr);
    let sections = &elf.section_headers;

    // the index of either `.bss` or `.data`
    let mut merged_sections = HashSet::new();
    let index = sections
        .iter()
        .position(|sect| {
            let name = elf.shdr_strtab.get(sect.sh_name).and_then(|res| res.ok());
            let bss_or_data = name == Some(".bss") || name == Some(".data");
            if bss_or_data {
                merged_sections.insert(name.unwrap().to_owned());
            }
            bss_or_data
        })
        .ok_or_else(|| anyhow!("linker sections `.bss` and `.data` not found"))?;

    let sect = &sections[index];
    // FIXME this is an over-estimate. When merging sections we should only
    // consider the alignment of the two potentially contiguous sections rather
    // than the maximum alignment among all of them
    let max_align = sections
        .iter()
        .map(|sect| sect.sh_addralign)
        .max()
        .unwrap_or(1);
    let mut start = sect.sh_addr;
    let mut total_size = sect.sh_size;

    // merge contiguous sections
    // first, grow backwards (towards smaller addresses)
    for sect in sections[..index].iter().rev() {
        let address = sect.sh_addr;
        let size = sect.sh_size;

        if address + size <= start && start - (address + size) <= max_align {
            let name = elf
                .shdr_strtab
                .get(sect.sh_name)
                .and_then(|res| res.ok())
                .ok_or_else(|| anyhow!("no name information for section {}", sect.sh_name))?;
            merged_sections.insert(name.to_owned());
            start = address;
            total_size += size;
        } else {
            // not a contiguous section
            break;
        }
    }

    // then, grow forwards (towards bigger addresses)
    for sect in sections[index..].iter().skip(1) {
        let address = sect.sh_addr;
        let size = sect.sh_size;

        if start + total_size <= address && address - (start + total_size) <= max_align {
            let name = elf
                .shdr_strtab
                .get(sect.sh_name)
                .and_then(|res| res.ok())
                .ok_or_else(|| anyhow!("no name information for section {}", sect.sh_name))?;
            merged_sections.insert(name.to_owned());
            total_size += size;
        } else {
            // not a contiguous section
            break;
        }
    }

    let stack_top = elf
        .syms
        .iter()
        .filter_map(|sym| {
            if elf.strtab.get(sym.st_name).and_then(|res| res.ok()) == Some("__stack_top__") {
                Some(sym.st_value)
            } else {
                None
            }
        })
        .next()
        .ok_or_else(|| anyhow!("symbol `__stack_top__` not found"))?;

    Ok(Boundaries {
        address: start,
        merged_sections,
        size: total_size,
        stack_top,
    })
}
