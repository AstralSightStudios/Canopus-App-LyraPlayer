/*
 * Constructor/destructor glue for the stock NuttX modlib loader.
 * The module is copied into writable RAM, so verifier-ambiguous constant words
 * can be decoded before Rust observes its read-only ELF input sections.
 */
#include <stdint.h>

__attribute__((section(".rodata"), used, aligned(4)))
const uint8_t canopus_rodata_anchor[4] = {0};
/* A unique, non-empty, 4-byte anchor. It must be a distinct string (not the
 * shared empty-string byte) so SHF_MERGE never folds it away or repositions it,
 * and it must be pinned below so --gc-sections keeps it in the very first
 * (fixup-less) link too — otherwise the .rodata.str1.1 layout shifts by one
 * byte between the layout pass and the final link and the verifier's
 * coincidental-address scan never converges. */
__attribute__((section(".rodata.str1.1"), used, aligned(4)))
const uint8_t canopus_rodata_str1_1_anchor[4] = {0xA7, 0x5C, 0xD3, 0x00};

extern void canopus_decode_opaque_words(void) __attribute__((weak));

__attribute__((constructor)) static void canopus_mod_ctor(void)
{
    extern int canopus_mod_prepare(const void *ctx);
    extern int canopus_register_module_descriptor(void);

    /* Pin both opaque-word anchors so they survive --gc-sections even before
     * the first fixup array exists. The asm has no runtime effect. */
    __asm__ volatile("" : : "r"(canopus_rodata_anchor),
                     "r"(canopus_rodata_str1_1_anchor) : "memory");

    if (canopus_decode_opaque_words != 0) {
        canopus_decode_opaque_words();
    }
    (void)canopus_mod_prepare(0);
    (void)canopus_register_module_descriptor();
}

__attribute__((destructor)) static void canopus_mod_dtor(void)
{
    extern int canopus_mod_stop(const void *ctx);
    (void)canopus_mod_stop(0);
}
