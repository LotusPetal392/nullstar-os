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

    mov r12, rax
    xor r13, r13
.Luppercase:
    cmp r13, r12
    jae .Lwrite_start
    mov al, byte ptr [rsp + r13]
    cmp al, 97
    jb .Luppercase_next
    cmp al, 122
    ja .Luppercase_next
    sub byte ptr [rsp + r13], 32
.Luppercase_next:
    inc r13
    jmp .Luppercase

.Lwrite_start:
    mov r14, rsp
    mov r15, r12
.Lwrite:
    mov rax, 1
    mov rdi, 1
    mov rsi, r14
    mov rdx, r15
    int 0x80
    test rax, rax
    jle .Lfailure
    add r14, rax
    sub r15, rax
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
