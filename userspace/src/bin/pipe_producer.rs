#![no_std]
#![no_main]

use core::{arch::global_asm, panic::PanicInfo};

global_asm!(
    r#"
    .section .text._start,"ax"
    .p2align 4
    .global _start
    .type _start,@function
_start:
    mov r12, 32
.Lyield:
    mov rax, 2
    int 0x80
    dec r12
    jnz .Lyield

    lea r12, [rip + message]
    mov r13, 42
.Lwrite:
    mov rax, 1
    mov rdi, 1
    mov rsi, r12
    mov rdx, r13
    int 0x80
    test rax, rax
    jle .Lfailure
    add r12, rax
    sub r13, rax
    jnz .Lwrite

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
    .ascii "Hello through a blocking GalacticOS pipe.\n"
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
