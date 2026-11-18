#import <AppKit/AppKit.h>
#include <dlfcn.h>
#include <errno.h>
#include <limits.h>
#include <signal.h>
#include <stdint.h>
#include <unistd.h>

// The matching JLI is loaded by absolute path from the selected Java runtime.
// These types match the exported OpenJDK 17 launcher ABI.
typedef struct { char **elements; size_t size; size_t capacity; } JliList;
typedef int (*JliLaunch)(int, char **, int, const char **, int, const char **,
                         const char *, const char *, const char *, const char *,
                         uint8_t, uint8_t, uint8_t, int);
static JliLaunch launch_java;

static int run_java(int count, char **arguments) {
    return launch_java(count, arguments, 0, NULL, 0, NULL,
                       "17", "17", "java", "java", 0, 1, 0, 0);
}

static int fail(const char *message) {
    fprintf(stderr, "Unable to start Auram's managed Java: %s\n", message);
    return 1;
}

static NSDictionary *read_invocation(void) {
    NSMutableData *body = [NSMutableData data];
    unsigned char bytes[8192];
    for (;;) {
        ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
        if (count == 0) break;
        if (count < 0 && errno == EINTR) continue;
        if (count < 0 || body.length + (size_t)count > 4 * 1024 * 1024) return nil;
        [body appendBytes:bytes length:(size_t)count];
    }
    id object = [NSJSONSerialization JSONObjectWithData:body options:0 error:NULL];
    return [object isKindOfClass:[NSDictionary class]] ? object : nil;
}

static BOOL prepare_application(void) {
    NSBundle *bundle = [NSBundle mainBundle];
    NSURL *icon = [bundle URLForResource:@"Auram" withExtension:@"png"];
    NSImage *unicorn = icon ? [[NSImage alloc] initWithContentsOfURL:icon] : nil;
    if (!unicorn) return NO;
    NSApplication *app = [NSApplication sharedApplication];
    [app setActivationPolicy:NSApplicationActivationPolicyRegular];
    [app setApplicationIconImage:unicorn];
    [unicorn release];

    // GLFW assumes an already-present NSApplication has its own menus.
    NSMenu *bar = [[[NSMenu alloc] init] autorelease];
    NSMenu *menu = [[[NSMenu alloc] initWithTitle:@"Auram"] autorelease];
    [[bar addItemWithTitle:@"Auram" action:NULL keyEquivalent:@""] setSubmenu:menu];
    [menu addItemWithTitle:@"About Auram" action:@selector(orderFrontStandardAboutPanel:) keyEquivalent:@""];
    [menu addItem:[NSMenuItem separatorItem]];
    [menu addItemWithTitle:@"Hide Auram" action:@selector(hide:) keyEquivalent:@"h"];
    [menu addItemWithTitle:@"Quit Auram" action:@selector(terminate:) keyEquivalent:@"q"];
    NSMenu *windows = [[[NSMenu alloc] initWithTitle:@"Window"] autorelease];
    [[bar addItemWithTitle:@"Window" action:NULL keyEquivalent:@""] setSubmenu:windows];
    [windows addItemWithTitle:@"Minimize" action:@selector(performMiniaturize:) keyEquivalent:@"m"];
    [app setWindowsMenu:windows];
    [app setMainMenu:bar];
    return YES;
}

int main(int argc, char **argv) {
    @autoreleasepool {
        // Zulu JLI calls the executable's main() again on a worker while it
        // parks the original thread in Cocoa. The callback already has the
        // expanded arguments; it must not read stdin or touch AppKit again.
        if (launch_java) return run_java(argc, argv);
        if (argc != 2 || strcmp(argv[1], "--auram-game-pipe") != 0)
            return fail("start this helper through Auram Launcher");
        // The launcher can close its log pipes while the game is still running.
        signal(SIGPIPE, SIG_IGN);
        NSDictionary *request = read_invocation();
        NSString *library = request[@"library"];
        NSString *glfw = request[@"glfw"];
        NSArray *arguments = request[@"arguments"];
        if (![library isKindOfClass:[NSString class]] || !library.isAbsolutePath ||
            ![glfw isKindOfClass:[NSString class]] || !glfw.isAbsolutePath ||
            ![arguments isKindOfClass:[NSArray class]])
            return fail("invalid game invocation");

        // Bundled applications otherwise have GLFW change into Contents/Resources.
        // LWJGL opens this same absolute library so the init hint stays in effect.
        void *glfw_library = dlopen(glfw.fileSystemRepresentation, RTLD_NOW | RTLD_LOCAL);
        if (!glfw_library) return fail(dlerror());
        void (*glfw_hint)(int, int) = dlsym(glfw_library, "glfwInitHint");
        if (!glfw_hint) return fail("the managed GLFW library does not support initialization hints");
        glfw_hint(0x00051001, 0); // GLFW_COCOA_CHDIR_RESOURCES, GLFW_FALSE

        void *jli = dlopen(library.fileSystemRepresentation, RTLD_NOW | RTLD_GLOBAL);
        if (!jli) return fail(dlerror());
        void (*init)(uint8_t, uint8_t) = dlsym(jli, "JLI_InitArgProcessing");
        JliList *(*list)(size_t) = dlsym(jli, "JLI_List_new");
        void (*add)(JliList *, char *) = dlsym(jli, "JLI_List_add");
        char *(*duplicate)(const char *) = dlsym(jli, "JLI_StringDup");
        JliList *(*preprocess)(const char *, uint8_t) = dlsym(jli, "JLI_PreprocessArg");
        uint8_t (*add_environment)(JliList *, const char *) = dlsym(jli, "JLI_AddArgsFromEnvVar");
        void (*release)(void *) = dlsym(jli, "JLI_MemFree");
        JliLaunch launch = dlsym(jli, "JLI_Launch");
        if (!init || !list || !add || !duplicate || !preprocess || !add_environment || !release || !launch)
            return fail("the selected Java runtime does not expose the expected JLI functions");

        init(0, 0);
        JliList *java_args = list(arguments.count + 2);
        add(java_args, duplicate(argv[0]));
        add_environment(java_args, "JDK_JAVA_OPTIONS");
        for (id argument in arguments) {
            if (![argument isKindOfClass:[NSString class]]) return fail("invalid argument type");
            const char *utf8 = [argument UTF8String];
            if (!utf8 || strlen(utf8) != [(NSString *)argument lengthOfBytesUsingEncoding:NSUTF8StringEncoding])
                return fail("invalid UTF-8 argument");
            JliList *expanded = preprocess(utf8, 1);
            if (!expanded) {
                add(java_args, duplicate(utf8));
                continue;
            }
            for (size_t index = 0; index < expanded->size; ++index)
                add(java_args, expanded->elements[index]);
            release(expanded->elements);
            release(expanded);
        }
        if (java_args->size > INT_MAX) return fail("too many game arguments");
        int count = (int)java_args->size;
        add(java_args, NULL);

        if (!prepare_application()) return fail("the packaged application icon is missing");
        launch_java = launch;
        return run_java(count, java_args->elements);
    }
}
