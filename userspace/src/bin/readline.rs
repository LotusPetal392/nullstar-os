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
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + prompt]
    mov rdx, 10
    int 0x80

    sub rsp, 256
    mov rax, 5
    xor rdi, rdi
    mov rsi, rsp
    mov rdx, 256
    int 0x80
    test rax, rax
    js .Lfailure
    mov r12, rax

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + response_prefix]
    mov rdx, 10
    int 0x80

    mov rax, 1
    mov rdi, 1
    mov rsi, rsp
    mov rdx, r12
    int 0x80
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

    .section .rodata,"a"
prompt:
    .ascii "readline> "
response_prefix:
    .ascii "terminal: "
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
