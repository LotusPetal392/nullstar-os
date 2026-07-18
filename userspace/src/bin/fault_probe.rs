#![no_std]
#![no_main]

use core::{arch::global_asm, panic::PanicInfo};

// This process deliberately faults so the kernel can verify per-process exception isolation.
global_asm!(
    r#"
    .section .text._start,"ax"
    .p2align 4
    .global _start
    .type _start,@function
_start:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + fault_message]
    mov rdx, 43
    int 0x80

    mov rax, 0x00000000dead0000
    mov rbx, qword ptr [rax]

    mov rax, 3
    mov rdi, 99
    int 0x80
    ud2
.size _start, .-_start

    .section .rodata,"a"
fault_message:
    .ascii "fault-probe: touching an unmapped page now\n"
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
