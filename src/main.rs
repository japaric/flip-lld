#![deny(warnings)]

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{anyhow, bail, Error};
use walkdir::WalkDir;
use xmas_elf::{sections::SectionData, symbol_table::Entry, ElfFile};

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
    // Allocatable linker section
    const SHF_ALLOC: u64 = 0x2;

    let bytes = &fs::read(path)?;
    let elf = &ElfFile::new(bytes).map_err(|s| anyhow!("{}", s))?;

    // sections that will be allocated in device memory
    let mut sections = elf
        .section_iter()
        .filter(|sect| sect.flags() & SHF_ALLOC == SHF_ALLOC)
        .collect::<Vec<_>>();
    sections.sort_by_key(|sect| sect.address());

    // the index of either `.bss` or `.data`
    let index = sections
        .iter()
        .position(|sect| {
            sect.get_name(elf)
                .map(|name| name == ".bss" || name == ".data")
                .unwrap_or(false)
        })
        .ok_or_else(|| anyhow!("linker sections `.bss` and `.data` not found"))?;

    let mut start = sections[index].address();
    let mut total_size = sections[index].size();

    // merge contiguous sections
    // first, grow backwards (towards smaller addresses)
    for sect in sections[..index].iter().rev() {
        let address = sect.address();
        let size = sect.size();

        if address + size == start {
            start = address;
            total_size += size;
        } else {
            // not a contiguous section
            break;
        }
    }

    // then, grow forwards (towards bigger addresses)
    for sect in sections[index..].iter().skip(1) {
        let address = sect.address();
        let size = sect.size();

        if start + total_size == address {
            start = address;
            total_size += size;
        } else {
            // not a contiguous section
            break;
        }
    }

    let maybe_stack_top = match elf
        .find_section_by_name(".symtab")
        .ok_or_else(|| anyhow!("`.symtab` section not found"))?
        .get_data(elf)
    {
        Ok(SectionData::SymbolTable32(entries)) => entries
            .iter()
            .filter_map(|entry| {
                if entry.get_name(elf) == Ok("__stack_top__") {
                    Some(entry.value())
                } else {
                    None
                }
            })
            .next(),

        Ok(SectionData::SymbolTable64(entries)) => entries
            .iter()
            .filter_map(|entry| {
                if entry.get_name(elf) == Ok("__stack_top__") {
                    Some(entry.value())
                } else {
                    None
                }
            })
            .next(),

        _ => bail!("`.symtab` data has the wrong format"),
    };

    let stack_top = maybe_stack_top.ok_or_else(|| anyhow!("symbol `__stack_top__` not found"))?;

    Ok(Boundaries {
        address: start,
        size: total_size,
        stack_top,
    })
}
