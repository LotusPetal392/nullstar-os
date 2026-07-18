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
    sub rsp, 256
.Lread:
    mov rax, 5
    xor rdi, rdi
    mov rsi, rsp
    mov rdx, 256
    int 0x80
    test rax, rax
    js .Lfailure
    jz .Ldone

    mov r12, rsp
    mov r13, rax
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
    jmp .Lread

.Ldone:
    add rsp, 256
    mov rax, 3
    xor rdi, rdi
    int 0x80
    ud2

.Lfailure:
    add rsp, 256
    mov rax, 3
    mov rdi, 1
    int 0x80
    ud2
.size _start, .-_start
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
