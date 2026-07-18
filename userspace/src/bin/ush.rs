#![no_std]
#![no_main]

use core::{arch::global_asm, panic::PanicInfo};

global_asm!(
    r#"
    .equ COMMAND_BYTES, 512
    .equ MAX_STAGES, 8
    .equ MAX_PIPES, MAX_STAGES - 1
    .equ STAGE_STARTS, COMMAND_BYTES
    .equ STAGE_LENGTHS, STAGE_STARTS + MAX_STAGES * 8
    .equ PIPE_READS, STAGE_LENGTHS + MAX_STAGES * 8
    .equ PIPE_WRITES, PIPE_READS + MAX_PIPES * 8
    .equ CHILD_PIDS, PIPE_WRITES + MAX_PIPES * 8
    .equ STACK_BYTES, 1024
    .equ PROMPT_BYTES, 5
    .equ HELP_BYTES, 92
    .equ SYNTAX_FAILURE_BYTES, 41
    .equ STAGE_FAILURE_BYTES, 40
    .equ PIPE_FAILURE_BYTES, 17
    .equ SPAWN_FAILURE_BYTES, 18
    .equ WAIT_FAILURE_BYTES, 17

    .section .text._start,"ax"
    .p2align 4
    .global _start
    .type _start,@function
_start:
    sub rsp, STACK_BYTES

.Lprompt:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + prompt]
    mov rdx, PROMPT_BYTES
    int 0x80

    mov rax, 5
    xor rdi, rdi
    mov rsi, rsp
    mov rdx, COMMAND_BYTES
    int 0x80
    test rax, rax
    js .Lfailure
    jz .Lexit_success
    mov r12, rax

.Ltrim_line_end:
    test r12, r12
    jz .Lprompt
    mov al, byte ptr [rsp + r12 - 1]
    cmp al, 10
    je .Ltrim_line_end_one
    cmp al, 13
    jne .Lparse_stages
.Ltrim_line_end_one:
    dec r12
    jmp .Ltrim_line_end

.Lparse_stages:
    xor r13, r13
    xor r14, r14
    xor r15, r15

.Lscan_stage:
    cmp r14, r12
    je .Lfinish_stage
    cmp byte ptr [rsp + r14], 124
    je .Lfinish_stage
    inc r14
    jmp .Lscan_stage

.Lfinish_stage:
    mov rbx, r13
    mov rbp, r14

.Ltrim_stage_start:
    cmp rbx, rbp
    je .Lpipeline_syntax
    mov al, byte ptr [rsp + rbx]
    cmp al, 32
    je .Ltrim_stage_start_one
    cmp al, 9
    jne .Ltrim_stage_end
.Ltrim_stage_start_one:
    inc rbx
    jmp .Ltrim_stage_start

.Ltrim_stage_end:
    cmp rbp, rbx
    je .Lpipeline_syntax
    mov al, byte ptr [rsp + rbp - 1]
    cmp al, 32
    je .Ltrim_stage_end_one
    cmp al, 9
    jne .Lstore_stage
.Ltrim_stage_end_one:
    dec rbp
    jmp .Ltrim_stage_end

.Lstore_stage:
    cmp r15, MAX_STAGES
    jae .Ltoo_many_stages
    lea rax, [rsp + rbx]
    mov qword ptr [rsp + STAGE_STARTS + r15 * 8], rax
    sub rbp, rbx
    mov qword ptr [rsp + STAGE_LENGTHS + r15 * 8], rbp
    inc r15

    cmp r14, r12
    je .Ldispatch
    inc r14
    mov r13, r14
    jmp .Lscan_stage

.Ldispatch:
    cmp r15, 1
    jne .Lcreate_pipeline

    mov rdi, qword ptr [rsp + STAGE_STARTS]
    mov rsi, qword ptr [rsp + STAGE_LENGTHS]
    cmp rsi, 4
    jne .Lspawn_single
    cmp dword ptr [rdi], 0x74697865
    je .Lexit_success
    cmp dword ptr [rdi], 0x706c6568
    je .Lhelp

.Lspawn_single:
    mov rax, 7
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

.Lcreate_pipeline:
    xor rbx, rbx
.Lcreate_pipe_loop:
    lea rbp, [r15 - 1]
    cmp rbx, rbp
    jae .Lspawn_pipeline

    mov rax, 10
    int 0x80
    test rax, rax
    js .Lpipe_create_failure

    mov edx, eax
    mov qword ptr [rsp + PIPE_READS + rbx * 8], rdx
    shr rax, 32
    mov qword ptr [rsp + PIPE_WRITES + rbx * 8], rax
    inc rbx
    jmp .Lcreate_pipe_loop

.Lspawn_pipeline:
    mov r14, r15
.Lspawn_stage_loop:
    test r14, r14
    jz .Lpipeline_spawned
    dec r14

    mov rdi, qword ptr [rsp + STAGE_STARTS + r14 * 8]
    mov rsi, qword ptr [rsp + STAGE_LENGTHS + r14 * 8]
    mov rdx, 2
    lea rax, [r15 - 1]
    cmp r14, rax
    jne .Lspawn_not_foreground
    or rdx, 1
.Lspawn_not_foreground:

    mov r10, -1
    test r14, r14
    jz .Lspawn_stdin_ready
    lea rax, [r14 - 1]
    mov r10, qword ptr [rsp + PIPE_READS + rax * 8]
.Lspawn_stdin_ready:

    mov r8, -1
    lea rax, [r15 - 1]
    cmp r14, rax
    je .Lspawn_stdout_ready
    mov r8, qword ptr [rsp + PIPE_WRITES + r14 * 8]
.Lspawn_stdout_ready:

    mov rax, 7
    int 0x80
    test rax, rax
    js .Lpipeline_spawn_failure
    mov qword ptr [rsp + CHILD_PIDS + r14 * 8], rax
    jmp .Lspawn_stage_loop

.Lpipeline_spawned:
    mov rbx, r15
    dec rbx
    xor rbp, rbp
.Lclose_pipeline_descriptors:
    cmp rbp, rbx
    jae .Lwait_pipeline

    mov rax, 6
    mov rdi, qword ptr [rsp + PIPE_READS + rbp * 8]
    int 0x80
    mov rax, 6
    mov rdi, qword ptr [rsp + PIPE_WRITES + rbp * 8]
    int 0x80
    inc rbp
    jmp .Lclose_pipeline_descriptors

.Lwait_pipeline:
    xor r14, r14
    xor r13, r13
.Lwait_stage_loop:
    cmp r14, r15
    jae .Lwait_pipeline_done
    mov rax, 8
    mov rdi, qword ptr [rsp + CHILD_PIDS + r14 * 8]
    int 0x80
    test rax, rax
    jns .Lwait_stage_next
    mov r13, 1
.Lwait_stage_next:
    inc r14
    jmp .Lwait_stage_loop

.Lwait_pipeline_done:
    test r13, r13
    jnz .Lwait_failure
    jmp .Lprompt

.Lpipe_create_failure:
    mov rbp, rbx
    xor r13, r13
.Lclose_created_pipes:
    cmp r13, rbp
    jae .Lpipe_failure
    mov rax, 6
    mov rdi, qword ptr [rsp + PIPE_READS + r13 * 8]
    int 0x80
    mov rax, 6
    mov rdi, qword ptr [rsp + PIPE_WRITES + r13 * 8]
    int 0x80
    inc r13
    jmp .Lclose_created_pipes

.Lpipeline_spawn_failure:
    mov r12, r14
    mov rbx, r15
    dec rbx
    xor rbp, rbp
.Lclose_failed_pipeline_descriptors:
    cmp rbp, rbx
    jae .Lwait_spawned_stages
    mov rax, 6
    mov rdi, qword ptr [rsp + PIPE_READS + rbp * 8]
    int 0x80
    mov rax, 6
    mov rdi, qword ptr [rsp + PIPE_WRITES + rbp * 8]
    int 0x80
    inc rbp
    jmp .Lclose_failed_pipeline_descriptors

.Lwait_spawned_stages:
    inc r12
.Lwait_spawned_stage_loop:
    cmp r12, r15
    jae .Lspawn_failure
    mov rax, 8
    mov rdi, qword ptr [rsp + CHILD_PIDS + r12 * 8]
    int 0x80
    inc r12
    jmp .Lwait_spawned_stage_loop

.Lhelp:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + help_message]
    mov rdx, HELP_BYTES
    int 0x80
    jmp .Lprompt

.Lpipeline_syntax:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + syntax_failure]
    mov rdx, SYNTAX_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Ltoo_many_stages:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + stage_failure]
    mov rdx, STAGE_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Lpipe_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + pipe_failure]
    mov rdx, PIPE_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Lspawn_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + spawn_failure]
    mov rdx, SPAWN_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Lwait_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + wait_failure]
    mov rdx, WAIT_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Lexit_success:
    add rsp, STACK_BYTES
    mov rax, 3
    xor rdi, rdi
    int 0x80
    ud2

.Lfailure:
    add rsp, STACK_BYTES
    mov rax, 3
    mov rdi, 1
    int 0x80
    ud2
.size _start, .-_start

    .section .rodata,"a"
prompt:
    .ascii "ush> "
prompt_end:
help_message:
    .ascii "builtins: help exit\nrun: cat /hello.txt\npipe: producer | filter | consumer (up to 8 stages)\n"
help_message_end:
syntax_failure:
    .ascii "ush: expected a non-empty pipeline stage\n"
syntax_failure_end:
stage_failure:
    .ascii "ush: pipeline supports at most 8 stages\n"
stage_failure_end:
pipe_failure:
    .ascii "ush: pipe failed\n"
pipe_failure_end:
spawn_failure:
    .ascii "ush: spawn failed\n"
spawn_failure_end:
wait_failure:
    .ascii "ush: wait failed\n"
wait_failure_end:
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
