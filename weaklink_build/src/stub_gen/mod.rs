use crate::util::iter_fmt;
use crate::SymbolStub;
use std::io::Write;

#[derive(PartialEq, Eq)]
pub(crate) enum TargetOs {
    Linux,
    MacOS,
    Windows,
}

pub(crate) trait StubGenerator {
    fn generate(&self, text: &mut dyn Write, symbols: &[SymbolStub], symbol_table: &str, sym_resolver: Option<&str>) {
        write_lines!(text,
            "global_asm!{{\""
            ".data"
            ".p2align 2, 0x0"
            "{pfx}{symbol_table}:"
            "{entries}"
            "\"}}",
            pfx = self.asm_symbol_prefix(),
            symbol_table = symbol_table,
            entries = iter_fmt(symbols.iter().enumerate(), |f, (idx, sym)| {
                let dir=self.data_ptr_directive();
                if sym_resolver.is_some() && !sym.is_data {
                    writeln!(f, "    {dir} resolve_{idx}")
                } else {
                    writeln!(f, "    {dir} 0")
                }
            }
        ));

        if let Some(sym_reslver) = sym_resolver {
            write_lines!(text,
                "global_asm!{{\""
                ".text"
                ".p2align 2, 0x0"
                //".type resolver_trampoline, function"
                "resolver_trampoline:");
            self.write_binder_stub(text, sym_reslver);
            writeln!(text, "\"}}");
        }

        for (i, symbol) in symbols.iter().enumerate() {
            if !symbol.is_data {
                write_lines!(text,
                    "global_asm!{{\""
                    ".text"
                    ".p2align 2, 0x0"
                    ".global \\\"{symbol}\\\"" // Will be unescaped the 2nd time when compiling the generated module.
                    //".type   \\\"{symbol}\\\", function"
                    "\\\"{symbol}\\\":",
                    symbol = symbol.export_name
                );
                self.write_fn_stub(text, symbol_table, i);

                if sym_resolver.is_some() {
                    writeln!(text, "resolve_{index}:", index = i);
                    self.write_jmp_binder(text, i, "resolver_trampoline");
                }

                writeln!(text, "\"}}");
            } else {
                write_lines!(text,
                    "#[no_mangle]"
                    "pub extern \"C\" fn {symbol}() -> Address {{"
                    "    unsafe {{ {symbol_table}[{index}] as Address }}"
                    "}}",
                    symbol = symbol.export_name,
                    symbol_table = symbol_table,
                    index = i
                );
            }
        }
    }

    /// Emit code that loads index'th entry from the symbol table and jumps to that address.
    fn write_fn_stub(&self, text: &mut dyn Write, symtab_base: &str, index: usize);

    /// Emit code that code that memoizes the value of `index`, then jumps to `binder`.
    fn write_jmp_binder(&self, text: &mut dyn Write, index: usize, binder: &str);

    /// Emit code that:
    /// - preserves all volatile registers, except IPCSR,
    /// - invokes `resolver`(see below), passing the value saved by write_jmp_binder() as a parameter,
    /// - restores volatile registers,
    /// - jumps to the address returned by `resolver`
    ///
    /// `"C" fn resolver(index: u32) -> usize`
    fn write_binder_stub(&self, text: &mut dyn Write, resolver: &str);

    /// Declaration directive for pointer-sized data.
    fn data_ptr_directive(&self) -> &str {
        ".quad"
    }

    /// A prefix, if any, that needs to be prepended to Rust symbols in order to reference them in assembly code.
    fn asm_symbol_prefix(&self) -> &str {
        ""
    }
}

pub(crate) mod aarch64;
pub(crate) mod arm;
pub(crate) mod x64;
pub(crate) mod loongarch64;
