/* Hermes payload layout: one flat image linked at address 0, entry first.
 * `cargo xtask payload` converts the ELF to that flat image and checks that
 * the entry is at offset 0 and that no relocation is left for a loader.
 * S1 (loader entry contract) may require changes here. */
ENTRY(_start)

SECTIONS
{
    . = 0;
    .text : {
        KEEP(*(.text.hermes_entry))
        *(.text .text.*)
    }
    .rodata : ALIGN(16) {
        *(.rodata .rodata.*)
    }
    .data : ALIGN(16) {
        *(.data .data.*)
    }
    .bss : ALIGN(16) {
        *(.bss .bss.*)
        *(COMMON)
    }
    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.comment)
        *(.note .note.*)
    }
}
