/*
 * Constructor/destructor glue for the stock NuttX modlib loader.
 * The module is copied into writable RAM, so verifier-ambiguous constant words
 * can be decoded before Rust observes its read-only ELF input sections.
 */
#include <stdint.h>

__attribute__((section(".rodata"), used, aligned(4)))
const uint8_t canopus_rodata_anchor[4] = {0};
__attribute__((section(".rodata.str1.1"), used, aligned(1)))
const uint8_t canopus_rodata_str1_1_anchor[1] = {0};

extern void canopus_decode_opaque_words(void) __attribute__((weak));

__attribute__((constructor)) static void canopus_mod_ctor(void)
{
    extern int canopus_mod_prepare(const void *ctx);
    extern const void *canopus_module_descriptor_ptr(void);

    if (canopus_decode_opaque_words != 0) {
        canopus_decode_opaque_words();
    }
    (void)canopus_module_descriptor_ptr();
    (void)canopus_mod_prepare(0);
}

__attribute__((destructor)) static void canopus_mod_dtor(void)
{
    extern int canopus_mod_stop(const void *ctx);
    (void)canopus_mod_stop(0);
}
