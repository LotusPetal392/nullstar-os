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
    lea rsi, [rip + first_message]
    mov rdx, first_message_end - first_message
    int 0x80

    mov rax, 2
    int 0x80

    mov rax, 1
    mov rdi, 1
    lea rsi, [rip + second_message]
    mov rdx, second_message_end - second_message
    int 0x80

    mov rax, 3
    mov rdi, 42
    int 0x80

    ud2
.size _start, .-_start

    .section .rodata,"a"
first_message:
    .ascii "userspace: hello from ring 3\n"
first_message_end:
second_message:
    .ascii "userspace: resumed after yield\n"
second_message_end:
"#,
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
