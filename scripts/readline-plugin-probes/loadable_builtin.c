/*
 * rl_probe: a tiny bash loadable builtin that exercises the in-process
 * readline C ABI (niubash#775, ruline#7/#8).
 *
 * It is deliberately a *probe*, not a plugin: every symbol is looked up with
 * dlsym(RTLD_DEFAULT) so the same .so can be loaded by GNU bash (where the
 * symbols come from libreadline) and by niu (where they must come from the
 * main executable).  A missing symbol is reported as `missing` instead of
 * failing to load, which is what lets the acceptance runner tell "the host
 * does not export the ABI yet" apart from "the fixture is broken".
 *
 * Build with scripts/readline-plugin-probes/build-builtin.sh.  Usage from a
 * host that supports `enable -f`:
 *
 *     enable -f /path/to/librl_probe.so rl_probe
 *     rl_probe
 *
 * Machine-readable stdout (stable field order, one line):
 *
 *     RL_PROBE version=0x0803 library=8.3 header=0x0803 add_defun=ok \
 *       bind_key=ok line_buffer=ok line_mutated=ok point=ok \
 *       point_mutated=ok result=ok
 *
 * Field values: ok | missing | error | absent | empty | skipped-empty.
 * `result=ok` means the version symbols, rl_add_defun and rl_bind_key are
 * present and callable.  line_buffer/point are reported separately because
 * a non-interactive `bash -c` may have no editor buffer even when the ABI is
 * otherwise live.
 *
 * `RL_PROBE_MUTATE=1` leaves the buffer/point mutations in place (the default
 * restores them) so a PTY test can prove the editor adopted the write-back.
 * `RL_PROBE_KEY_MARK=1` makes the rl_add_defun callback print a marker when
 * the host dispatches its key, proving a plugin binding fires live.
 *
 * The builtin struct is the stable GNU bash loadable-builtin contract
 * (builtins.h): name, function, flags, long_doc, short_doc, handle.  It is
 * declared here rather than including bash headers so the fixture builds on
 * hosts that only ship the readline headers.
 */

/* stdio.h must precede readline.h: the readline headers use FILE but do not
 * include stdio themselves (the project that includes them is expected to). */
#include <stdio.h>

#if defined(__has_include)
#  if __has_include(<readline/readline.h>)
#    include <readline/readline.h>
#    define RL_PROBE_HAVE_READLINE_HEADER 1
#  endif
#endif

#include <dlfcn.h>
#include <stdlib.h>
#include <string.h>

#ifndef RL_PROBE_HAVE_READLINE_HEADER
/* Minimal readline declarations, used only when the headers are absent. */
typedef int rl_command_func_t(int, int);
#endif

#ifndef RL_READLINE_VERSION
#define RL_READLINE_VERSION 0
#endif

/* GNU bash builtins.h (the ABI is stable; keep the field order). */
typedef struct word_list {
    struct word_list *next;
    char *word;
} WORD_LIST;

typedef int sh_builtin_func_t(WORD_LIST *);

struct builtin {
    char *name;
    sh_builtin_func_t *function;
    int flags;
    char *const *long_doc;
    const char *short_doc;
    char *handle;
};

#define BUILTIN_ENABLED 0x01

/* A high keymap slot (0xfe) so a live probe session does not steal a
 * printable key that a human might press next. */
#define RL_PROBE_KEY 0xfe

/* A function registered through rl_add_defun.  The default proves only that
 * registration returns success.  With RL_PROBE_KEY_MARK=1 it prints a marker
 * when the host's live editor dispatches the key, which is how the PTY probe
 * proves a plugin binding actually fires through the host keymap. */
static int rl_probe_defun(int count, int key)
{
    const char *mark = getenv("RL_PROBE_KEY_MARK");
    if (mark != NULL && mark[0] != '\0' && strcmp(mark, "0") != 0) {
        printf("RL_PROBE_DEFUN key=%d count=%d\n", key, count);
        fflush(stdout);
    }
    return 0;
}

typedef int (*rl_add_defun_fn)(const char *, rl_command_func_t *, int);
typedef int (*rl_bind_key_fn)(int, rl_command_func_t *);

static void *probe_sym(const char *name)
{
    return dlsym(RTLD_DEFAULT, name);
}

static int rl_probe_builtin(WORD_LIST *list)
{
    const char *version = "missing";
    const char *library = "missing";
    const char *add_defun = "missing";
    const char *bind_key = "missing";
    const char *line_buffer = "absent";
    const char *line_mutated = "absent";
    const char *point = "absent";
    const char *point_mutated = "absent";
    const char *mutate = "off";
    int leave_mutation;
    char version_buffer[32];
    char library_buffer[128];

    (void)list;

    /* Acceptance probe mode: the default restores every mutation so the
     * host's line survives a plugin call unchanged.  With RL_PROBE_MUTATE=1
     * the first-byte flip and point bump are left in place, which lets the
     * PTY probe prove the interactive editor adopts the plugin's write-back
     * (niubash#775), not merely that the fixture can see the live storage. */
    {
        const char *value = getenv("RL_PROBE_MUTATE");
        leave_mutation = value != NULL && value[0] != '\0' && strcmp(value, "0") != 0;
        if (leave_mutation) {
            mutate = "on";
        }
    }

    {
        int *value = (int *)probe_sym("rl_readline_version");
        if (value != NULL) {
            snprintf(version_buffer, sizeof(version_buffer), "0x%04x",
                     (unsigned)(*value) & 0xffffu);
            version = version_buffer;
        }
    }

    {
        const char **value = (const char **)probe_sym("rl_library_version");
        if (value != NULL && *value != NULL) {
            snprintf(library_buffer, sizeof(library_buffer), "%.80s", *value);
            library = library_buffer;
        }
    }

    {
        rl_add_defun_fn add =
            (rl_add_defun_fn)probe_sym("rl_add_defun");
        if (add != NULL) {
            add_defun = add("rl-probe-defun", rl_probe_defun, RL_PROBE_KEY) == 0
                            ? "ok"
                            : "error";
        }
    }

    {
        rl_bind_key_fn bind = (rl_bind_key_fn)probe_sym("rl_bind_key");
        if (bind != NULL) {
            bind_key =
                bind(RL_PROBE_KEY, rl_probe_defun) == 0 ? "ok" : "error";
        }
    }

    {
        char **buffer = (char **)probe_sym("rl_line_buffer");
        if (buffer != NULL && *buffer != NULL) {
            char *line = *buffer;
            line_buffer = "ok";
            if (line[0] == '\0') {
                line_mutated = "skipped-empty";
            } else {
                /* Modify the live buffer in place and restore it: this
                 * proves the symbol addresses the editor's storage, not a
                 * private copy, without depending on rl_insert_text. */
                char original = line[0];
                line[0] = (original == 'X') ? 'Y' : 'X';
                line_mutated = (line[0] != original) ? "ok" : "error";
                if (!leave_mutation) {
                    line[0] = original;
                }
            }
        }
    }

    {
        int *value = (int *)probe_sym("rl_point");
        if (value != NULL) {
            int original = *value;
            point = "ok";
            *value = original + 1;
            point_mutated = (*value == original + 1) ? "ok" : "error";
            if (!leave_mutation) {
                *value = original;
            }
        }
    }

    printf("RL_PROBE version=%s library=%s header=0x%04x "
           "add_defun=%s bind_key=%s line_buffer=%s line_mutated=%s "
           "point=%s point_mutated=%s mutate=%s result=%s\n",
           version, library, (unsigned)RL_READLINE_VERSION, add_defun,
           bind_key, line_buffer, line_mutated, point, point_mutated,
           mutate,
           (strcmp(version, "missing") != 0 && strcmp(library, "missing") != 0 &&
            strcmp(add_defun, "ok") == 0 && strcmp(bind_key, "ok") == 0)
               ? "ok"
               : "missing-abi");
    fflush(stdout);
    return 0;
}

static char *const rl_probe_doc[] = {
    "Probe the in-process readline C ABI (niubash#775, ruline#7/#8).",
    "Prints one machine-readable RL_PROBE line and returns 0.",
    NULL,
};

struct builtin rl_probe_struct = {
    .name = "rl_probe",
    .function = rl_probe_builtin,
    .flags = BUILTIN_ENABLED,
    .long_doc = rl_probe_doc,
    .short_doc = "rl_probe - print readline ABI acceptance data",
    .handle = NULL,
};
