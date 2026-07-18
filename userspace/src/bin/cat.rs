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
    mov rax, qword ptr [rsp]
    cmp rax, 2
    jb .Lusage

    mov r12, qword ptr [rsp + 16]
    xor r13, r13
.Lpath_length:
    cmp byte ptr [r12 + r13], 0
    je .Lopen
    inc r13
    cmp r13, 4096
    jb .Lpath_length
    jmp .Lfailure

.Lopen:
    mov rax, 4
    mov rdi, r12
    mov rsi, r13
    xor rdx, rdx
    int 0x80
    test rax, rax
    js .Lfailure
    mov r14, rax

    sub rsp, 1024
.Lread:
    mov rax, 5
    mov rdi, r14
    mov rsi, rsp
    mov rdx, 1024
    int 0x80
    test rax, rax
    js .Lread_failure
    jz .Ldone

    mov rdx, rax
    mov rax, 1
    mov rdi, 1
    mov rsi, rsp
    int 0x80
    test rax, rax
    js .Lread_failure
    jmp .Lread

.Ldone:
    mov rax, 6
    mov rdi, r14
    int 0x80
    add rsp, 1024
    mov rax, 3
    xor rdi, rdi
    int 0x80
    ud2

.Lread_failure:
    mov rax, 6
    mov rdi, r14
    int 0x80
    add rsp, 1024
    jmp .Lfailure

.Lusage:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + usage_message]
    mov rdx, 19
    int 0x80
    mov rax, 3
    mov rdi, 64
    int 0x80
    ud2

.Lfailure:
    mov rax, 3
    mov rdi, 1
    int 0x80
    ud2
.size _start, .-_start

    .section .rodata,"a"
usage_message:
    .ascii "usage: /cat <path>\n"
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
