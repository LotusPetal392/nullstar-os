#![no_std]
#![no_main]

use core::{arch::global_asm, panic::PanicInfo};

global_asm!(
    r#"
    .equ COMMAND_BYTES, 512
    .equ MAX_STAGES, 8
    .equ MAX_PIPES, MAX_STAGES - 1
    .equ MAX_JOBS, 4
    .equ STAGE_STARTS, COMMAND_BYTES
    .equ STAGE_LENGTHS, STAGE_STARTS + MAX_STAGES * 8
    .equ PIPE_READS, STAGE_LENGTHS + MAX_STAGES * 8
    .equ PIPE_WRITES, PIPE_READS + MAX_PIPES * 8
    .equ CHILD_PIDS, PIPE_WRITES + MAX_PIPES * 8
    .equ JOB_STAGE_COUNTS, CHILD_PIDS + MAX_STAGES * 8
    .equ JOB_STATES, JOB_STAGE_COUNTS + MAX_JOBS * 8
    .equ JOB_GROUPS, JOB_STATES + MAX_JOBS * 8
    .equ JOB_PIDS, JOB_GROUPS + MAX_JOBS * 8
    .equ CURRENT_JOB_SLOT, JOB_PIDS + MAX_JOBS * MAX_STAGES * 8
    .equ BACKGROUND_FLAG, CURRENT_JOB_SLOT + 8
    .equ CURRENT_GROUP, BACKGROUND_FLAG + 8
    .equ STACK_BYTES, 1280
    .equ PROMPT_BYTES, 5
    .equ HELP_BYTES, 174
    .equ SYNTAX_FAILURE_BYTES, 41
    .equ STAGE_FAILURE_BYTES, 40
    .equ JOB_LIMIT_FAILURE_BYTES, 34
    .equ BUILTIN_BACKGROUND_FAILURE_BYTES, 43
    .equ KILL_USAGE_BYTES, 19
    .equ KILL_MISSING_BYTES, 30
    .equ KILL_FAILURE_BYTES, 17
    .equ INTERRUPTED_BYTES, 17
    .equ PIPE_FAILURE_BYTES, 17
    .equ SPAWN_FAILURE_BYTES, 18
    .equ WAIT_FAILURE_BYTES, 17
    .equ WAIT_COMPLETE_BYTES, 30
    .equ NO_JOBS_BYTES, 24
    .equ JOB_PREFIX_BYTES, 1
    .equ JOB_SUFFIX_BYTES, 2
    .equ JOB_RUNNING_BYTES, 8
    .equ JOB_DONE_BYTES, 5
    .equ JOB_FAILED_BYTES, 7
    .equ JOB_STARTED_BYTES, 8
    .equ JOB_SIGNALED_BYTES, 9

    .equ SPAWN_FOREGROUND, 1
    .equ SPAWN_USE_DESCRIPTORS, 2
    .equ SPAWN_NEW_PROCESS_GROUP, 4
    .equ SPAWN_JOIN_PROCESS_GROUP, 8
    .equ SIGNAL_TERMINATE, 15

    .section .text._start,"ax"
    .p2align 4
    .global _start
    .type _start,@function
_start:
    sub rsp, STACK_BYTES
    mov r9, rsp

    xor rax, rax
    xor rcx, rcx
.Lclear_jobs:
    cmp rcx, MAX_JOBS * (3 + MAX_STAGES)
    jae .Lprompt
    mov qword ptr [r9 + JOB_STAGE_COUNTS + rcx * 8], rax
    inc rcx
    jmp .Lclear_jobs

.Lprompt:
    mov qword ptr [r9 + CURRENT_JOB_SLOT], -1
    mov qword ptr [r9 + BACKGROUND_FLAG], 0
    mov qword ptr [r9 + CURRENT_GROUP], -1

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + prompt]
    mov rdx, PROMPT_BYTES
    int 0x80

    mov rax, 5
    xor rdi, rdi
    mov rsi, r9
    mov rdx, COMMAND_BYTES
    int 0x80
    test rax, rax
    js .Lfailure
    jz .Lexit_success
    mov r12, rax

.Ltrim_line_end:
    test r12, r12
    jz .Lprompt
    mov al, byte ptr [r9 + r12 - 1]
    cmp al, 10
    je .Ltrim_line_end_one
    cmp al, 13
    je .Ltrim_line_end_one
    cmp al, 32
    je .Ltrim_line_end_one
    cmp al, 9
    jne .Ldetect_background
.Ltrim_line_end_one:
    dec r12
    jmp .Ltrim_line_end

.Ldetect_background:
    cmp byte ptr [r9 + r12 - 1], 38
    jne .Lparse_stages
    mov qword ptr [r9 + BACKGROUND_FLAG], 1
    dec r12
.Ltrim_background_end:
    test r12, r12
    jz .Lpipeline_syntax
    mov al, byte ptr [r9 + r12 - 1]
    cmp al, 32
    je .Ltrim_background_one
    cmp al, 9
    jne .Lparse_stages
.Ltrim_background_one:
    dec r12
    jmp .Ltrim_background_end

.Lparse_stages:
    xor r13, r13
    xor r14, r14
    xor r15, r15

.Lscan_stage:
    cmp r14, r12
    je .Lfinish_stage
    cmp byte ptr [r9 + r14], 124
    je .Lfinish_stage
    inc r14
    jmp .Lscan_stage

.Lfinish_stage:
    mov rbx, r13
    mov rbp, r14

.Ltrim_stage_start:
    cmp rbx, rbp
    je .Lpipeline_syntax
    mov al, byte ptr [r9 + rbx]
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
    mov al, byte ptr [r9 + rbp - 1]
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
    lea rax, [r9 + rbx]
    mov qword ptr [r9 + STAGE_STARTS + r15 * 8], rax
    sub rbp, rbx
    mov qword ptr [r9 + STAGE_LENGTHS + r15 * 8], rbp
    inc r15

    cmp r14, r12
    je .Ldispatch
    inc r14
    mov r13, r14
    jmp .Lscan_stage

.Ldispatch:
    cmp r15, 1
    jne .Lreserve_if_background

    mov rdi, qword ptr [r9 + STAGE_STARTS]
    mov rsi, qword ptr [r9 + STAGE_LENGTHS]
    cmp rsi, 4
    jne .Lcheck_kill_builtin
    cmp dword ptr [rdi], 0x74697865
    je .Lbuiltin_exit
    cmp dword ptr [rdi], 0x706c6568
    je .Lbuiltin_help
    cmp dword ptr [rdi], 0x73626f6a
    je .Lbuiltin_jobs
    cmp dword ptr [rdi], 0x74696177
    je .Lbuiltin_wait
    cmp dword ptr [rdi], 0x6c6c696b
    je .Lbuiltin_kill

.Lcheck_kill_builtin:
    cmp rsi, 6
    jb .Lreserve_if_background
    cmp dword ptr [rdi], 0x6c6c696b
    jne .Lreserve_if_background
    mov al, byte ptr [rdi + 4]
    cmp al, 32
    je .Lbuiltin_kill
    cmp al, 9
    je .Lbuiltin_kill

.Lreserve_if_background:
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    je .Ldispatch_ready
    call .Lreserve_job
    test rax, rax
    js .Ljob_limit

.Ldispatch_ready:
    cmp r15, 1
    jne .Lcreate_pipeline

.Lspawn_single:
    mov rdi, qword ptr [r9 + STAGE_STARTS]
    mov rsi, qword ptr [r9 + STAGE_LENGTHS]
    mov rdx, SPAWN_NEW_PROCESS_GROUP | SPAWN_FOREGROUND
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    je .Lspawn_single_flags_ready
    mov rdx, SPAWN_NEW_PROCESS_GROUP
.Lspawn_single_flags_ready:
    mov r10, -1
    mov r8, -1
    mov rbx, -1
    mov rax, 7
    int 0x80
    test rax, rax
    js .Lspawn_failure_release
    mov r13, rax

    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    je .Lwait_single

    mov r12, qword ptr [r9 + CURRENT_JOB_SLOT]
    mov qword ptr [r9 + JOB_STAGE_COUNTS + r12 * 8], 1
    mov qword ptr [r9 + JOB_STATES + r12 * 8], 0
    mov qword ptr [r9 + JOB_GROUPS + r12 * 8], r13
    mov rax, r12
    shl rax, 6
    mov qword ptr [r9 + JOB_PIDS + rax], r13
    mov r13, 3
    call .Lprint_job_line
    jmp .Lprompt

.Lwait_single:
    mov rax, 8
    mov rdi, r13
    int 0x80
    test rax, rax
    js .Lwait_failure
    cmp rax, 130
    je .Linterrupted
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
    mov qword ptr [r9 + PIPE_READS + rbx * 8], rdx
    shr rax, 32
    mov qword ptr [r9 + PIPE_WRITES + rbx * 8], rax
    inc rbx
    jmp .Lcreate_pipe_loop

.Lspawn_pipeline:
    mov r14, r15
.Lspawn_stage_loop:
    test r14, r14
    jz .Lpipeline_spawned
    dec r14

    mov rdi, qword ptr [r9 + STAGE_STARTS + r14 * 8]
    mov rsi, qword ptr [r9 + STAGE_LENGTHS + r14 * 8]
    mov rdx, SPAWN_USE_DESCRIPTORS
    lea rax, [r15 - 1]
    cmp r14, rax
    jne .Lspawn_join_group
    or rdx, SPAWN_NEW_PROCESS_GROUP
    mov rbx, -1
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    jne .Lspawn_group_ready
    or rdx, SPAWN_FOREGROUND
    jmp .Lspawn_group_ready
.Lspawn_join_group:
    or rdx, SPAWN_JOIN_PROCESS_GROUP
    mov rbx, qword ptr [r9 + CURRENT_GROUP]
.Lspawn_group_ready:

    mov r10, -1
    test r14, r14
    jz .Lspawn_stdin_ready
    lea rax, [r14 - 1]
    mov r10, qword ptr [r9 + PIPE_READS + rax * 8]
.Lspawn_stdin_ready:

    mov r8, -1
    lea rax, [r15 - 1]
    cmp r14, rax
    je .Lspawn_stdout_ready
    mov r8, qword ptr [r9 + PIPE_WRITES + r14 * 8]
.Lspawn_stdout_ready:

    mov rax, 7
    int 0x80
    test rax, rax
    js .Lpipeline_spawn_failure
    mov qword ptr [r9 + CHILD_PIDS + r14 * 8], rax
    lea rdx, [r15 - 1]
    cmp r14, rdx
    jne .Lspawn_stage_loop
    mov qword ptr [r9 + CURRENT_GROUP], rax
    jmp .Lspawn_stage_loop

.Lpipeline_spawned:
    mov rbx, r15
    dec rbx
    xor rbp, rbp
.Lclose_pipeline_descriptors:
    cmp rbp, rbx
    jae .Lpipeline_descriptors_closed

    mov rax, 6
    mov rdi, qword ptr [r9 + PIPE_READS + rbp * 8]
    int 0x80
    mov rax, 6
    mov rdi, qword ptr [r9 + PIPE_WRITES + rbp * 8]
    int 0x80
    inc rbp
    jmp .Lclose_pipeline_descriptors

.Lpipeline_descriptors_closed:
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    je .Lwait_pipeline

    mov r12, qword ptr [r9 + CURRENT_JOB_SLOT]
    mov qword ptr [r9 + JOB_STAGE_COUNTS + r12 * 8], r15
    mov qword ptr [r9 + JOB_STATES + r12 * 8], 0
    mov rax, qword ptr [r9 + CURRENT_GROUP]
    mov qword ptr [r9 + JOB_GROUPS + r12 * 8], rax
    mov rax, r12
    shl rax, 6
    xor r13, r13
.Lcopy_background_pids:
    cmp r13, r15
    jae .Lbackground_pipeline_started
    mov rdx, qword ptr [r9 + CHILD_PIDS + r13 * 8]
    lea rdi, [r9 + JOB_PIDS]
    add rdi, rax
    mov qword ptr [rdi + r13 * 8], rdx
    inc r13
    jmp .Lcopy_background_pids

.Lbackground_pipeline_started:
    mov r13, 3
    call .Lprint_job_line
    jmp .Lprompt

.Lwait_pipeline:
    xor r14, r14
    xor r13, r13
.Lwait_stage_loop:
    cmp r14, r15
    jae .Lwait_pipeline_done
    mov rax, 8
    mov rdi, qword ptr [r9 + CHILD_PIDS + r14 * 8]
    int 0x80
    test rax, rax
    js .Lwait_stage_error
    cmp rax, 130
    jne .Lwait_stage_next
    mov r13, 2
    jmp .Lwait_stage_next
.Lwait_stage_error:
    mov r13, 1
.Lwait_stage_next:
    inc r14
    jmp .Lwait_stage_loop

.Lwait_pipeline_done:
    cmp r13, 1
    je .Lwait_failure
    cmp r13, 2
    je .Linterrupted
    jmp .Lprompt

.Lpipe_create_failure:
    mov rbp, rbx
    xor r13, r13
.Lclose_created_pipes:
    cmp r13, rbp
    jae .Lpipe_failure_release
    mov rax, 6
    mov rdi, qword ptr [r9 + PIPE_READS + r13 * 8]
    int 0x80
    mov rax, 6
    mov rdi, qword ptr [r9 + PIPE_WRITES + r13 * 8]
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
    mov rdi, qword ptr [r9 + PIPE_READS + rbp * 8]
    int 0x80
    mov rax, 6
    mov rdi, qword ptr [r9 + PIPE_WRITES + rbp * 8]
    int 0x80
    inc rbp
    jmp .Lclose_failed_pipeline_descriptors

.Lwait_spawned_stages:
    inc r12
.Lwait_spawned_stage_loop:
    cmp r12, r15
    jae .Lspawn_failure_release
    mov rax, 8
    mov rdi, qword ptr [r9 + CHILD_PIDS + r12 * 8]
    int 0x80
    inc r12
    jmp .Lwait_spawned_stage_loop

.Lbuiltin_exit:
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    jne .Lbuiltin_background_failure
    call .Lwait_all_jobs
    jmp .Lexit_success

.Lbuiltin_help:
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    jne .Lbuiltin_background_failure
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + help_message]
    mov rdx, HELP_BYTES
    int 0x80
    jmp .Lprompt

.Lbuiltin_jobs:
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    jne .Lbuiltin_background_failure
    call .Lpoll_jobs
    jmp .Lprompt

.Lbuiltin_wait:
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    jne .Lbuiltin_background_failure
    call .Lwait_all_jobs
    test rax, rax
    jnz .Lwait_failure
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + wait_complete]
    mov rdx, WAIT_COMPLETE_BYTES
    int 0x80
    jmp .Lprompt

.Lreserve_job:
    xor rax, rax
.Lreserve_job_loop:
    cmp rax, MAX_JOBS
    jae .Lreserve_job_failure
    cmp qword ptr [r9 + JOB_STAGE_COUNTS + rax * 8], 0
    je .Lreserve_job_found
    inc rax
    jmp .Lreserve_job_loop
.Lreserve_job_found:
    mov qword ptr [r9 + JOB_STAGE_COUNTS + rax * 8], -1
    mov qword ptr [r9 + JOB_STATES + rax * 8], 0
    mov qword ptr [r9 + CURRENT_JOB_SLOT], rax
    ret
.Lreserve_job_failure:
    mov rax, -1
    ret

.Lrelease_current_job:
    mov rax, qword ptr [r9 + CURRENT_JOB_SLOT]
    test rax, rax
    js .Lrelease_current_job_done
    mov qword ptr [r9 + JOB_STAGE_COUNTS + rax * 8], 0
    mov qword ptr [r9 + JOB_STATES + rax * 8], 0
    mov qword ptr [r9 + JOB_GROUPS + rax * 8], 0
    mov qword ptr [r9 + CURRENT_JOB_SLOT], -1
.Lrelease_current_job_done:
    ret

.Lpoll_jobs:
    xor r12, r12
    xor r15, r15
.Lpoll_job_loop:
    cmp r12, MAX_JOBS
    jae .Lpoll_jobs_done
    mov rbp, qword ptr [r9 + JOB_STAGE_COUNTS + r12 * 8]
    test rbp, rbp
    jle .Lpoll_job_next
    mov r15, 1
    mov r13, qword ptr [r9 + JOB_STATES + r12 * 8]
    test r13, r13
    jnz .Lpoll_job_print

    xor rcx, rcx
    xor r14, r14
    xor rbx, rbx
    mov rax, r12
    shl rax, 6
    mov rsi, rax
.Lpoll_child_loop:
    cmp rcx, rbp
    jae .Lpoll_children_done
    mov rax, 11
    lea rdi, [r9 + JOB_PIDS]
    add rdi, rsi
    mov rdi, qword ptr [rdi + rcx * 8]
    int 0x80
    test rax, rax
    jns .Lpoll_child_complete
    cmp rax, -11
    je .Lpoll_child_running
    mov rbx, 1
    jmp .Lpoll_child_next
.Lpoll_child_running:
    mov r14, 1
    jmp .Lpoll_child_next
.Lpoll_child_complete:
    test rax, rax
    jz .Lpoll_child_next
    mov rbx, 1
.Lpoll_child_next:
    inc rcx
    jmp .Lpoll_child_loop

.Lpoll_children_done:
    test r14, r14
    jnz .Lpoll_job_running
    mov r13, 1
    test rbx, rbx
    jz .Lpoll_job_store_state
    mov r13, 2
    jmp .Lpoll_job_store_state
.Lpoll_job_running:
    xor r13, r13
.Lpoll_job_store_state:
    mov qword ptr [r9 + JOB_STATES + r12 * 8], r13

.Lpoll_job_print:
    call .Lprint_job_line
.Lpoll_job_next:
    inc r12
    jmp .Lpoll_job_loop

.Lpoll_jobs_done:
    test r15, r15
    jnz .Lpoll_jobs_return
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + no_jobs]
    mov rdx, NO_JOBS_BYTES
    int 0x80
.Lpoll_jobs_return:
    ret

.Lwait_all_jobs:
    xor r12, r12
    xor rbx, rbx
.Lwait_job_loop:
    cmp r12, MAX_JOBS
    jae .Lwait_all_jobs_done
    mov rbp, qword ptr [r9 + JOB_STAGE_COUNTS + r12 * 8]
    test rbp, rbp
    jle .Lwait_job_next

    xor rcx, rcx
    mov rax, r12
    shl rax, 6
    mov rsi, rax
.Lwait_job_child_loop:
    cmp rcx, rbp
    jae .Lwait_job_clear
    mov rax, 8
    lea rdi, [r9 + JOB_PIDS]
    add rdi, rsi
    mov rdi, qword ptr [rdi + rcx * 8]
    int 0x80
    test rax, rax
    jns .Lwait_job_child_next
    mov rbx, 1
.Lwait_job_child_next:
    inc rcx
    jmp .Lwait_job_child_loop

.Lwait_job_clear:
    mov qword ptr [r9 + JOB_STAGE_COUNTS + r12 * 8], 0
    mov qword ptr [r9 + JOB_STATES + r12 * 8], 0
    mov qword ptr [r9 + JOB_GROUPS + r12 * 8], 0
.Lwait_job_next:
    inc r12
    jmp .Lwait_job_loop
.Lwait_all_jobs_done:
    mov rax, rbx
    ret

.Lprint_job_line:
    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + job_prefix]
    mov rdx, JOB_PREFIX_BYTES
    int 0x80

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + job_digits]
    add rsi, r12
    mov rdx, 1
    int 0x80

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + job_suffix]
    mov rdx, JOB_SUFFIX_BYTES
    int 0x80

    cmp r13, 1
    je .Lprint_job_done
    cmp r13, 2
    je .Lprint_job_failed
    cmp r13, 3
    je .Lprint_job_started
    cmp r13, 4
    je .Lprint_job_signaled
    lea rsi, [rip + job_running]
    mov rdx, JOB_RUNNING_BYTES
    jmp .Lprint_job_status
.Lprint_job_done:
    lea rsi, [rip + job_done]
    mov rdx, JOB_DONE_BYTES
    jmp .Lprint_job_status
.Lprint_job_failed:
    lea rsi, [rip + job_failed]
    mov rdx, JOB_FAILED_BYTES
    jmp .Lprint_job_status
.Lprint_job_started:
    lea rsi, [rip + job_started]
    mov rdx, JOB_STARTED_BYTES
    jmp .Lprint_job_status
.Lprint_job_signaled:
    lea rsi, [rip + job_signaled]
    mov rdx, JOB_SIGNALED_BYTES
.Lprint_job_status:
    mov rax, 1
    mov rdi, 1
    int 0x80
    ret

.Lbuiltin_kill:
    cmp qword ptr [r9 + BACKGROUND_FLAG], 0
    jne .Lbuiltin_background_failure
    cmp rsi, 4
    jbe .Lkill_usage
    add rdi, 5
    sub rsi, 5
.Lkill_skip_space:
    test rsi, rsi
    jz .Lkill_usage
    mov al, byte ptr [rdi]
    cmp al, 32
    je .Lkill_skip_one
    cmp al, 9
    jne .Lkill_optional_percent
.Lkill_skip_one:
    inc rdi
    dec rsi
    jmp .Lkill_skip_space
.Lkill_optional_percent:
    cmp al, 37
    jne .Lkill_parse_digit
    inc rdi
    dec rsi
.Lkill_parse_digit:
    cmp rsi, 1
    jne .Lkill_usage
    movzx r12d, byte ptr [rdi]
    sub r12, 49
    cmp r12, MAX_JOBS
    jae .Lkill_usage
    cmp qword ptr [r9 + JOB_STAGE_COUNTS + r12 * 8], 0
    jle .Lkill_missing
    mov rax, 12
    mov rdi, qword ptr [r9 + JOB_GROUPS + r12 * 8]
    mov rsi, SIGNAL_TERMINATE
    int 0x80
    test rax, rax
    js .Lkill_failure
    mov qword ptr [r9 + JOB_STATES + r12 * 8], 4
    mov r13, 4
    call .Lprint_job_line
    jmp .Lprompt

.Lkill_usage:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + kill_usage]
    mov rdx, KILL_USAGE_BYTES
    int 0x80
    jmp .Lprompt

.Lkill_missing:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + kill_missing]
    mov rdx, KILL_MISSING_BYTES
    int 0x80
    jmp .Lprompt

.Lkill_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + kill_failure]
    mov rdx, KILL_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Linterrupted:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + interrupted]
    mov rdx, INTERRUPTED_BYTES
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

.Ljob_limit:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + job_limit_failure]
    mov rdx, JOB_LIMIT_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Lbuiltin_background_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + builtin_background_failure]
    mov rdx, BUILTIN_BACKGROUND_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Lpipe_failure_release:
    call .Lrelease_current_job
.Lpipe_failure:
    mov rax, 1
    mov rdi, 2
    lea rsi, [rip + pipe_failure]
    mov rdx, PIPE_FAILURE_BYTES
    int 0x80
    jmp .Lprompt

.Lspawn_failure_release:
    call .Lrelease_current_job
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
    .ascii "builtins: help jobs wait kill %N exit\nbackground: command & (up to 4 jobs)\nCtrl-C: interrupt foreground process group\npipeline: producer | filter | consumer (up to 8 stages)\n"
help_message_end:
syntax_failure:
    .ascii "ush: expected a non-empty pipeline stage\n"
syntax_failure_end:
stage_failure:
    .ascii "ush: pipeline supports at most 8 stages\n"
stage_failure_end:
job_limit_failure:
    .ascii "ush: background job table is full\n"
job_limit_failure_end:
builtin_background_failure:
    .ascii "ush: builtins cannot run in the background\n"
builtin_background_failure_end:
kill_usage:
    .ascii "usage: kill %1..%4\n"
kill_usage_end:
kill_missing:
    .ascii "ush: background job not found\n"
kill_missing_end:
kill_failure:
    .ascii "ush: kill failed\n"
kill_failure_end:
interrupted:
    .ascii "ush: interrupted\n"
interrupted_end:
pipe_failure:
    .ascii "ush: pipe failed\n"
pipe_failure_end:
spawn_failure:
    .ascii "ush: spawn failed\n"
spawn_failure_end:
wait_failure:
    .ascii "ush: wait failed\n"
wait_failure_end:
wait_complete:
    .ascii "ush: background jobs complete\n"
wait_complete_end:
no_jobs:
    .ascii "ush: no background jobs\n"
no_jobs_end:
job_prefix:
    .ascii "["
job_prefix_end:
job_digits:
    .ascii "1234"
job_suffix:
    .ascii "] "
job_suffix_end:
job_running:
    .ascii "running\n"
job_running_end:
job_done:
    .ascii "done\n"
job_done_end:
job_failed:
    .ascii "failed\n"
job_failed_end:
job_started:
    .ascii "started\n"
job_started_end:
job_signaled:
    .ascii "signaled\n"
job_signaled_end:
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
