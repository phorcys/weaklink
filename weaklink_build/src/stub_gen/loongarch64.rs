use super::TargetOs;
use crate::SymbolStub;
use std::io::{Read, Write};

pub struct LoongArch64StubGenerator {
    pub(crate) target_os: TargetOs,
}

impl super::StubGenerator for LoongArch64StubGenerator {
    fn write_fn_stub(&self, text: &mut dyn Write, symtab_base: &str, index: usize) {
        write_lines!(text,

            "    la.global $r12, {symtab_base}"
            "    addu16i.d $r12, $r12, {offset}"
            "    jirl $r0, $r12, 0",
            symtab_base = symtab_base,
            offset = index * 8
        );
    }
    
    fn write_jmp_binder(&self, text: &mut dyn Write, index: usize, binder: &str) {
        write_lines!(text,
            "    li.d $a0, {index}"
            "    b {binder}",
            index=index,
            binder=binder
        );
    }

    fn write_binder_stub(&self, text: &mut dyn Write, resolver: &str) {
        write_lines!(text,
            "    .cfi_startproc"
            "    addi.d $sp, $sp, -144"
            "    st.d $ra, $sp, 136"
            "    st.d $a0, $sp, 128"
            "    st.d $a1, $sp, 120"
            "    st.d $a2, $sp, 112"
            "    st.d $a3, $sp, 104"
            "    st.d $a4, $sp, 96"
            "    st.d $a5, $sp, 88"
            "    st.d $a6, $sp, 80"
            "    st.d $a7, $sp, 72"
            "    st.d $t0, $sp, 64"
            "    st.d $t1, $sp, 56"
            "    st.d $t2, $sp, 48"
            "    st.d $t3, $sp, 40"
            "    st.d $t4, $sp, 32"
            "    st.d $t5, $sp, 24"
            "    st.d $t6, $sp, 16"
            "    st.d $t7, $sp, 8"
            "    st.d $t8, $sp, 0"
            
            // 调用解析器
            "    move $a0, $r21"  // LoongArch通常用$r21作为特殊寄存器存地址
            "    bl {resolver}"
            "    move $r21, $a0"  // 将结果存回专用寄存器
            
            // 恢复寄存器
            "    ld.d $t8, $sp, 0"
            "    ld.d $t7, $sp, 8"
            "    ld.d $t6, $sp, 16"
            "    ld.d $t5, $sp, 24"
            "    ld.d $t4, $sp, 32"
            "    ld.d $t3, $sp, 40"
            "    ld.d $t2, $sp, 48"
            "    ld.d $t1, $sp, 56"
            "    ld.d $t0, $sp, 64"
            "    ld.d $a7, $sp, 72"
            "    ld.d $a6, $sp, 80"
            "    ld.d $a5, $sp, 88"
            "    ld.d $a4, $sp, 96"
            "    ld.d $a3, $sp, 104"
            "    ld.d $a2, $sp, 112"
            "    ld.d $a1, $sp, 120"
            "    ld.d $a0, $sp, 128"
            "    ld.d $ra, $sp, 136"
            "    addi.d $sp, $sp, 144"
            
            // 跳转到目标地址
            "    jirl $r0, $r21, 0"
            "    .cfi_endproc",
            resolver=resolver
        );
    }
}
