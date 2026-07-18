#![no_std]
#![no_main]

use core::{arch::global_asm, panic::PanicInfo};

global_asm!(
    r#"
    .equ DELAY_YIELDS, 64
    .equ MESSAGE_BYTES, 24

    .section .text._start,"ax"
    .p2align 4
    .global _start
    .type _start,@function
_start:
    mov r12, DELAY_YIELDS
.Lyield:
    mov rax, 2
    int 0x80
    dec r12
    jnz .Lyield

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + message]
    mov rdx, MESSAGE_BYTES
    int 0x80
    test rax, rax
    js .Lfailure

    mov rax, 3
    xor rdi, rdi
    int 0x80
    ud2

.Lfailure:
    mov rax, 3
    mov rdi, 1
    int 0x80
    ud2
.size _start, .-_start

    .section .rodata,"a"
message:
    .ascii "background job complete\n"
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
