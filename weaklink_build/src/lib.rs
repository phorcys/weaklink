#![allow(unused)]
#![doc = include_str!("../README.md")]

macro_rules! write_lines {
    ($dest:expr, $($line:literal)+ $(, $name:ident=$value:expr)*) => (write!($dest, concat!($($line,"\n"),+) $(, $name=$value)*))
}

pub mod exports;
pub mod imports;
mod stub_gen;
mod util;

use std::borrow::{Cow, ToOwned};
use std::collections::{hash_map::Entry, HashMap};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::{env, fmt};

use util::iter_fmt;

use crate::stub_gen::TargetOs;

type Error = Box<dyn std::error::Error>;

#[derive(Clone, Default, Debug)]
pub struct SymbolStub {
    /// Symbol name exported by the wrapped library.
    pub import_name: String,
    /// Symbol name that will be exported from the stub library.
    pub export_name: String,
    /// If true, generate a function that returns symbol address when called.
    pub is_data: bool,
}

impl SymbolStub {
    pub fn new(name: &str) -> SymbolStub {
        SymbolStub {
            import_name: name.to_string(),
            export_name: name.to_string(),
            is_data: false,
        }
    }

    pub fn new_data(exp_name: &str, imp_name: &str) -> SymbolStub {
        SymbolStub {
            export_name: exp_name.to_string(),
            import_name: imp_name.to_string(),
            is_data: true,
        }
    }
}

pub struct Config {
    /// Name of the static variable that exposes management API in the generated stubs crate.
    pub name: String,
    /// Target triple to generate code for.
    pub target: String,
    /// Dylib names to try when loading implicitly.
    pub dylib_names: Vec<String>,
    /// Whether to perform symbol name adjustment. 
    /// 
    /// Currently this handles a quirk of MacOSX linker, which automatically adds leading underscores to all exports.
    pub adjust_symbol_names: bool,
    /// Whether to support lazy binding.
    /// 
    /// Laxy binding allows calling functions from the wrapped library without resolving them ahead of time.
    /// The cons of laxy binding are:
    /// - It is not possible to gracefully handle missing library or symbols.  Should this happen, the program will panic.
    /// - Adds some overhead to each call.
    pub lazy_binding: bool,
    // The list of symbol stubs created so far.
    stubs: Vec<SymbolStub>,
    // Look up index in `stubs` by the export name.
    stub_by_exp: HashMap<String, usize>,
    // Group name => stub indices in `stubs`.
    groups: HashMap<String, Vec<usize>>,
}

impl Config {
    /// Create a new build configuration with the following defaults
    /// - [`name`](`Config::name`): The `name` parameter.
    /// - [`target`](`Config::target`): The current cargo build target.
    /// - [`dylib_names`](`Config::dylib_names`): An empty vector.
    /// - [`adjust_symbol_names`](`Config::adjust_symbol_names`): `true`
    /// - [`lazy_binding`](`Config::lazy_binding`): `false`
    pub fn new(name: &str) -> Self {
        let target = match env::var("TARGET") {
            Ok(target) => target,
            Err(_) => env!("TARGET").to_string(), // Fall back to host target
        };

        Config {
            name: name.into(),
            target: target,
            dylib_names: vec![],
            adjust_symbol_names: true,
            lazy_binding: false,
            stubs: Vec::new(),
            stub_by_exp: HashMap::new(),
            groups: HashMap::new(),
        }
    }

    /// Add a group of symbols that may be resolved all at once using the specified group name.  
    /// A symbol may appear in more than one group.
    pub fn add_symbol_group<'a>(
        &mut self,
        group_name: &str,
        symbols: impl IntoIterator<Item = SymbolStub>,
    ) -> Result<(), Error> {
        if let Some(_) = self.groups.get(group_name) {
            Err(format!("Group \"{group_name}\" already exists"))?;
        }
        let mut group_syms = Vec::new();
        for symbol in symbols {
            let sym_idx = match self.stub_by_exp.entry(symbol.export_name.clone()) {
                Entry::Occupied(o) => {
                    let idx = *o.get();
                    let existing = &self.stubs[idx];
                    if existing.is_data != symbol.is_data {
                        return Err(format!(
                            "Stub for symbol '{}' already exists, but with a different `is_data` value: {}",
                            existing.export_name, existing.is_data
                        )
                        .into());
                    }
                    if self.stubs[idx].import_name != symbol.import_name {
                        return Err(format!(
                            "Stub for symbol '{}' already exists, but with a different `import_name` value: {}",
                            existing.export_name, existing.import_name
                        )
                        .into());
                    }
                    idx
                }
                Entry::Vacant(v) => {
                    let idx = self.stubs.len();
                    self.stubs.push(symbol);
                    v.insert(idx);
                    idx
                }
            };
            group_syms.push(sym_idx);
        }
        self.groups.insert(group_name.to_string(), group_syms);
        Ok(())
    }

    /// Generate source of the stub crate.
    pub fn generate_source(&self, text: &mut dyn Write) {
        // Adjust names for MacOS ABI
        let mut stubs = Cow::from(&self.stubs);
        if self.adjust_symbol_names && self.target.contains("-apple-") {
            let new_stubs = self
                .stubs
                .iter()
                .map(|stub| {
                    let mut stub = stub.clone();
                    if !stub.is_data && stub.export_name == stub.import_name {
                        if stub.export_name.starts_with('_') {
                            stub.import_name.remove(0);
                        } else {
                            stub.export_name.insert(0, '_');
                        }
                    }
                    stub
                })
                .collect::<Vec<_>>();
            stubs = Cow::from(new_stubs);
        }

        // Header
        write_lines!(text,
            "#[allow(unused_imports)]"
            "use weaklink::{{Library, Group, Address}};"
            "use core::arch::global_asm;"
            "use std::ffi::CStr;"
        );

        // Declare symbol table (will be defined by StubGenerator)
        let sym_table = format!("symbol_table_{:08x}", rand::random::<u64>());
        write_lines!(text,
            "extern \"C\" {{"
            "    static {sym_table}: [Address; {size}];"
            "}}",
            sym_table=sym_table,
            size=stubs.len()
        );

        // Emit library object
        write_lines!(text,
            "#[no_mangle]"
            "#[allow(non_upper_case_globals)]"
            "pub static {name}: Library = Library::new("
            "    &[{dylib_names}],"
            "    unsafe {{ &[\n{symbol_names}] }},"
            "    unsafe {{ &{sym_table} }},"
            ");",
            name = self.name,
            dylib_names = iter_fmt(&self.dylib_names, |f, name| write!(f, "\"{name}\",")),
            symbol_names = iter_fmt(stubs.as_ref().iter().enumerate(), |f, (i, sym)|
                writeln!(f, "      CStr::from_bytes_with_nul_unchecked(b\"{}\\0\"), // {i}", sym.import_name)),
            sym_table=sym_table
        );

        // Emit group objects
        for (grp_name, indices) in &self.groups {
            let mut indices = indices.clone();
            indices.sort();
            write_lines!(text,
                "#[no_mangle]"
                "#[allow(non_upper_case_globals)]"
                "pub static {grp_name}: Group = Group::new("
                "    &{name},"
                "    &[{indices}],"
                ");",
                name = self.name,
                grp_name = grp_name,
                indices = iter_fmt(indices, |f, idx| write!(f, "{idx},"))
            );
        }

        let target_os = if self.target.contains("linux") {
            TargetOs::Linux
        } else if self.target.contains("apple") {
            TargetOs::MacOS
        } else if self.target.contains("windows") {
            TargetOs::Windows
        } else {
            panic!("Unsupported OS");
        };

        // Emit symbol table and PLT
        let stub_gen: Box<dyn stub_gen::StubGenerator> = if self.target.starts_with("x86_64-") {
            Box::new(stub_gen::x64::X64StubGenerator { target_os })
        } else if self.target.starts_with("aarch64-") {
            Box::new(stub_gen::aarch64::Aarch64StubGenerator { target_os })
        } else if self.target.starts_with("arm") {
            Box::new(stub_gen::arm::ArmStubGenerator {})
        } else if self.target.starts_with("loongarch64") {
            Box::new(stub_gen::loongarch64::LoongArch64StubGenerator { target_os })
        } else {
            panic!("Unsupported arch");
        };

        let sym_resolver = if self.lazy_binding {
            let sym_resolver = format!("resolver_{:08x}", rand::random::<u64>());
            write_lines!(text,
                "#[no_mangle]"
                "extern \"C\" fn {sym_resolver}(index: u32) -> Address {{"
                "    {name}.lazy_resolve(index)"
                "}}",
                name = self.name,
                sym_resolver=sym_resolver
            );
            Some(sym_resolver)
        } else {
            None
        };

        stub_gen.generate(text, stubs.as_ref(), &sym_table, sym_resolver.as_deref());
    }
}
