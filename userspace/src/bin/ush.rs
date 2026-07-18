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

.Lprompt:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + prompt]
    mov rdx, 5
    int 0x80

    mov rax, 5
    xor rdi, rdi
    mov rsi, rsp
    mov rdx, 256
    int 0x80
    test rax, rax
    js .Lfailure
    jz .Lexit_success
    mov r12, rax

.Ltrim:
    test r12, r12
    jz .Lprompt
    mov al, byte ptr [rsp + r12 - 1]
    cmp al, 10
    je .Ltrim_one
    cmp al, 13
    jne .Ldispatch
.Ltrim_one:
    dec r12
    jmp .Ltrim

.Ldispatch:
    cmp r12, 4
    jne .Lspawn
    cmp dword ptr [rsp], 0x74697865
    je .Lexit_success
    cmp dword ptr [rsp], 0x706c6568
    je .Lhelp

.Lspawn:
    mov rax, 7
    mov rdi, rsp
    mov rsi, r12
    mov rdx, 1
    int 0x80
    test rax, rax
    js .Lspawn_failure
    mov r13, rax

    mov rax, 8
    mov rdi, r13
    int 0x80
    test rax, rax
    js .Lwait_failure
    jmp .Lprompt

.Lhelp:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + help_message]
    mov rdx, 40
    int 0x80
    jmp .Lprompt

.Lspawn_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + spawn_failure]
    mov rdx, 18
    int 0x80
    jmp .Lprompt

.Lwait_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + wait_failure]
    mov rdx, 17
    int 0x80
    jmp .Lprompt

.Lexit_success:
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
    .ascii "ush> "
help_message:
    .ascii "builtins: help exit\nrun: cat /hello.txt\n"
spawn_failure:
    .ascii "ush: spawn failed\n"
wait_failure:
    .ascii "ush: wait failed\n"
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
