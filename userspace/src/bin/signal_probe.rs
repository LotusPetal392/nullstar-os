#![no_std]
#![no_main]

use core::{arch::global_asm, panic::PanicInfo};

global_asm!(
    r#"
    .equ MESSAGE_BYTES, 21

    .section .text._start,"ax"
    .p2align 4
    .global _start
    .type _start,@function
_start:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + message]
    mov rdx, MESSAGE_BYTES
    int 0x80
    test rax, rax
    js .Lfailure

.Lyield_forever:
    mov rax, 2
    int 0x80
    jmp .Lyield_forever

.Lfailure:
    mov rax, 3
    mov rdi, 1
    int 0x80
    ud2
.size _start, .-_start

    .section .rodata,"a"
message:
    .ascii "signal probe running\n"
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
