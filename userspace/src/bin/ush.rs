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
    sub rsp, 512

.Lprompt:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + prompt]
    mov rdx, 5
    int 0x80

    mov rax, 5
    xor rdi, rdi
    mov rsi, rsp
    mov rdx, 512
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
    jne .Lfind_pipe
    cmp dword ptr [rsp], 0x74697865
    je .Lexit_success
    cmp dword ptr [rsp], 0x706c6568
    je .Lhelp

.Lfind_pipe:
    xor r14, r14
.Lfind_pipe_loop:
    cmp r14, r12
    je .Lspawn_single
    cmp byte ptr [rsp + r14], 124
    je .Lpipeline
    inc r14
    jmp .Lfind_pipe_loop

.Lspawn_single:
    mov rax, 7
    mov rdi, rsp
    mov rsi, r12
    mov rdx, 1
    mov r10, -1
    mov r8, -1
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

.Lpipeline:
    mov r11, r14
.Ltrim_left:
    test r11, r11
    jz .Lpipeline_syntax
    mov al, byte ptr [rsp + r11 - 1]
    cmp al, 32
    je .Ltrim_left_one
    cmp al, 9
    jne .Lright_start
.Ltrim_left_one:
    dec r11
    jmp .Ltrim_left

.Lright_start:
    lea rbx, [rsp + r14 + 1]
    mov rbp, r12
    sub rbp, r14
    dec rbp
.Ltrim_right_start:
    test rbp, rbp
    jz .Lpipeline_syntax
    mov al, byte ptr [rbx]
    cmp al, 32
    je .Ltrim_right_start_one
    cmp al, 9
    jne .Ltrim_right_end
.Ltrim_right_start_one:
    inc rbx
    dec rbp
    jmp .Ltrim_right_start

.Ltrim_right_end:
    test rbp, rbp
    jz .Lpipeline_syntax
    mov al, byte ptr [rbx + rbp - 1]
    cmp al, 32
    je .Ltrim_right_end_one
    cmp al, 9
    jne .Lcreate_pipe
.Ltrim_right_end_one:
    dec rbp
    jmp .Ltrim_right_end

.Lcreate_pipe:
    mov rax, 10
    int 0x80
    test rax, rax
    js .Lpipe_failure
    mov r14d, eax
    shr rax, 32
    mov r15d, eax

    mov rax, 7
    mov rdi, rbx
    mov rsi, rbp
    mov rdx, 3
    mov r10, r14
    mov r8, -1
    int 0x80
    test rax, rax
    js .Lconsumer_spawn_failure
    mov r13, rax

    mov rax, 7
    mov rdi, rsp
    mov rsi, r11
    mov rdx, 2
    mov r10, -1
    mov r8, r15
    int 0x80
    test rax, rax
    js .Lproducer_spawn_failure
    mov r12, rax

    mov rax, 6
    mov rdi, r14
    int 0x80
    mov rax, 6
    mov rdi, r15
    int 0x80

    mov rax, 8
    mov rdi, r12
    int 0x80
    test rax, rax
    js .Lwait_failure

    mov rax, 8
    mov rdi, r13
    int 0x80
    test rax, rax
    js .Lwait_failure
    jmp .Lprompt

.Lconsumer_spawn_failure:
    mov rax, 6
    mov rdi, r14
    int 0x80
    mov rax, 6
    mov rdi, r15
    int 0x80
    jmp .Lspawn_failure

.Lproducer_spawn_failure:
    mov rax, 6
    mov rdi, r14
    int 0x80
    mov rax, 6
    mov rdi, r15
    int 0x80
    mov rax, 8
    mov rdi, r13
    int 0x80
    jmp .Lspawn_failure

.Lhelp:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + help_message]
    mov rdx, 76
    int 0x80
    jmp .Lprompt

.Lpipeline_syntax:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + syntax_failure]
    mov rdx, 36
    int 0x80
    jmp .Lprompt

.Lpipe_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + pipe_failure]
    mov rdx, 17
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
    add rsp, 512
    mov rax, 3
    xor rdi, rdi
    int 0x80
    ud2

.Lfailure:
    add rsp, 512
    mov rax, 3
    mov rdi, 1
    int 0x80
    ud2
.size _start, .-_start

    .section .rodata,"a"
prompt:
    .ascii "ush> "
help_message:
    .ascii "builtins: help exit\nrun: cat /hello.txt\npipe: pipe-producer | pipe-consumer\n"
syntax_failure:
    .ascii "ush: expected `producer | consumer`\n"
pipe_failure:
    .ascii "ush: pipe failed\n"
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
