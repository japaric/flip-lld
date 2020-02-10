#![deny(warnings)]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, Error};
use goblin::elf::Elf;
use walkdir::WalkDir;

fn main() -> Result<(), Error> {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    let rust_lld = rust_lld()?;

    // run the linker exactly as `rustc` instructed
    if !Command::new(&rust_lld).args(&args).status()?.success() {
        return Err(anyhow!("first `rust-lld` invocation failed"));
    }

    // retrieve the output file name
    let output = &get_o_value(&args)?;
    let Boundaries {
        stack_top,
        address,
        size,
    } = get_boundaries(output)?;

    if stack_top > address + size {
        let mut new_boundary = stack_top - size;

        // 8-byte align the new boundary; most architectures need the stack to
        // be 4-byte or 8-byte aligned
        let rem = new_boundary % 8;
        if rem != 0 {
            new_boundary -= rem;
        }
        // swap the location of the stack and the statically allocated data so
        // they can't run into each other
        args.insert(2, format!("--defsym=__ram_start__={}", new_boundary));
        args.insert(2, format!("--defsym=__stack_top__={}", new_boundary));

        let mut c = Command::new(&rust_lld);
        c.args(&args);
        if !c.status()?.success() {
            return Err(anyhow!("second `rust-lld` invocation failed"));
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
    let index = sections
        .iter()
        .position(|sect| {
            let name = elf.shdr_strtab.get(sect.sh_name).and_then(|res| res.ok());
            name == Some(".bss") || name == Some(".data")
        })
        .ok_or_else(|| anyhow!("linker sections `.bss` and `.data` not found"))?;

    let sect = &sections[index];
    let align = sections
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

        if address + size <= start && start - (address + size) <= align {
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

        if start + total_size <= address && address - (start + total_size) <= align {
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
        size: total_size,
        stack_top,
    })
}
