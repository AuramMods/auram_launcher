import com.sun.jna.Function;
import com.sun.jna.Memory;
import com.sun.jna.NativeLibrary;
import com.sun.jna.Pointer;
import com.sun.jna.WString;
import com.sun.jna.ptr.PointerByReference;
import java.nio.file.Files;
import java.nio.file.Path;
import java.io.ByteArrayInputStream;
import javax.imageio.ImageIO;
import java.util.Objects;
import org.lwjgl.glfw.GLFWNativeCocoa;
import org.lwjgl.glfw.GLFWNativeWin32;

import static org.lwjgl.glfw.GLFW.*;

/** Native GLFW probe used with the actual AuramCore startup agent and managed JVM. */
public class AuramProbe {
    private static final boolean MAC = System.getProperty("os.name").startsWith("Mac");
    private static final String GAME_ID = "art.arcane.auram.game";

    public static void main(String[] args) throws Exception {
        require(args[3].equals("Unicode 雪 with spaces, quotes \" and backslash \\"), "game arguments changed");
        Path icon = Path.of(args[4]);
        boolean branded = Boolean.parseBoolean(args[2]);
        require(Objects.equals(System.getProperty("auram.branding.icon"), branded ? icon.toString() : null),
                "branding icon argument changed");
        require(glfwInit(), "GLFW initialization failed");
        glfwWindowHint(GLFW_CLIENT_API, GLFW_NO_API);
        glfwWindowHint(GLFW_FOCUSED, GLFW_FALSE);
        long window = glfwCreateWindow(420, 150, "Original loading title", 0, 0);
        require(window != 0, "GLFW did not create a window");
        String initial = title(window);
        require(initial.equals(branded ? "Auram" : "Original loading title"), "initial window title: " + initial);
        String identity = MAC ? checkMac() : checkWindows(window);
        glfwSetWindowTitle(window, "Attempted replacement title");
        String later = title(window);
        require(later.equals(branded ? "Auram" : "Attempted replacement title"), "later window title: " + later);
        require((MAC ? checkMac() : checkWindows(window)).equals(identity), "native branding changed");
        Files.writeString(Path.of(args[0]), "pid=" + ProcessHandle.current().pid() + "\nfirst=" + initial
                + "\nlater=" + later + "\n" + identity + "\njava=" + System.getProperty("java.runtime.version"));
        System.out.println("Auram probe stdout");
        System.err.println("Auram probe stderr");
        if (args.length > 5) System.out.println("Login probe: " + args[5]);
        for (int count = 0; count < 450 && !Files.exists(Path.of(args[1])); count++) {
            glfwPollEvents();
            Thread.sleep(100);
        }
        glfwDestroyWindow(window);
        glfwTerminate();
        System.exit(23);
    }

    private static String title(long window) {
        if (MAC) {
            Pointer cocoa = new Pointer(GLFWNativeCocoa.glfwGetCocoaWindow(window));
            return text(send(cocoa, "title"));
        }
        Pointer hwnd = new Pointer(GLFWNativeWin32.glfwGetWin32Window(window));
        Memory result = new Memory(1024);
        user32("GetWindowTextW").invokeInt(new Object[]{hwnd, result, 512});
        return result.getWideString(0);
    }

    private static String checkMac() throws Exception {
        Function main = NativeLibrary.getInstance("System").getFunction("pthread_main_np");
        require(main.invokeInt(new Object[]{}) == 1, "JVM did not run on the Cocoa first thread");
        Pointer bundle = send(cls("NSBundle"), "mainBundle");
        Memory cwd = new Memory(4096);
        NativeLibrary.getInstance("System").getFunction("getcwd").invokePointer(new Object[]{cwd, 4096L});
        require(Path.of(cwd.getString(0)).toRealPath().equals(Path.of(System.getProperty("user.dir")).toRealPath()),
                "GLFW changed the native working directory to " + cwd.getString(0));
        Pointer app = send(cls("NSApplication"), "sharedApplication");
        String identifier = text(send(bundle, "bundleIdentifier"));
        require(GAME_ID.equals(identifier), "unexpected game bundle " + identifier);
        Pointer info = send(bundle, "infoDictionary");
        for (String key : new String[]{"CFBundleName", "CFBundleDisplayName", "CFBundleExecutable"}) {
            require("Auram".equals(text(send(info, "objectForKey:", nsString(key)))), "unexpected " + key);
        }
        require("Auram".equals(text(send(send(cls("NSProcessInfo"), "processInfo"), "processName"))), "wrong process name");
        Pointer running = send(cls("NSRunningApplication"), "currentApplication");
        String name = text(send(running, "localizedName"));
        require("Auram".equals(name), "wrong Launch Services name: " + name);
        Pointer mainMenu = send(app, "mainMenu");
        long items = objc().invokeLong(new Object[]{mainMenu, selector("numberOfItems")});
        boolean aboutAuram = false;
        for (long index = 0; index < items; ++index) {
            Pointer submenu = send(send(mainMenu, "itemAtIndex:", index), "submenu");
            if (submenu == null || objc().invokeLong(new Object[]{submenu, selector("numberOfItems")}) == 0)
                continue;
            Pointer first = send(submenu, "itemAtIndex:", 0L);
            aboutAuram |= "About Auram".equals(text(send(first, "title")));
        }
        require(aboutAuram, "app menu has no About Auram item");
        String icon = text(send(bundle, "pathForResource:ofType:", nsString("Auram"), nsString("png")));
        var expected = ImageIO.read(Path.of(icon).toFile());
        Pointer tiff = send(send(app, "applicationIconImage"), "TIFFRepresentation");
        int length = objc().invokeInt(new Object[]{tiff, selector("length")});
        var actual = ImageIO.read(new ByteArrayInputStream(send(tiff, "bytes").getByteArray(0, length)));
        require(actual.getWidth() == expected.getWidth() && actual.getHeight() == expected.getHeight(),
                "AppKit icon dimensions differ");
        for (int y = 0; y < expected.getHeight(); ++y) {
            for (int x = 0; x < expected.getWidth(); ++x) {
                int a = actual.getRGB(x, y);
                int b = expected.getRGB(x, y);
                for (int channel = 0; channel <= 24; channel += 8) {
                    require(Math.abs(premultiplied(a, channel) - premultiplied(b, channel)) <= 2,
                            "AppKit icon differs from the unicorn");
                }
            }
        }
        return "bundle=" + identifier + "\nappMenu=" + name + "\nfirstThread=true\nicon=unicorn";
    }

    private static int premultiplied(int pixel, int channel) {
        int value = (pixel >>> channel) & 255;
        return channel == 24 ? value : (value * (pixel >>> 24) + 127) / 255;
    }

    private static String checkWindows(long window) {
        NativeLibrary shell = NativeLibrary.getInstance("shell32");
        PointerByReference value = new PointerByReference();
        int hresult = shell.getFunction("GetCurrentProcessExplicitAppUserModelID", Function.ALT_CONVENTION)
                .invokeInt(new Object[]{value});
        require(hresult == 0 && GAME_ID.equals(value.getValue().getWideString(0)), "wrong taskbar identity");
        NativeLibrary.getInstance("ole32").getFunction("CoTaskMemFree", Function.ALT_CONVENTION)
                .invokeVoid(new Object[]{value.getValue()});
        Pointer exe = NativeLibrary.getInstance("kernel32").getFunction("GetModuleHandleW", Function.ALT_CONVENTION)
                .invokePointer(new Object[]{null});
        Pointer expected = user32("LoadImageW").invokePointer(
                new Object[]{exe, new WString("GLFW_ICON"), 1, 0, 0, 0x8040});
        Pointer hwnd = new Pointer(GLFWNativeWin32.glfwGetWin32Window(window));
        Pointer actual = user32("GetClassLongPtrW").invokePointer(new Object[]{hwnd, -14});
        require(expected != null && expected.equals(actual), "first HWND does not use GLFW_ICON");
        return "taskbarId=" + GAME_ID + "\nicon=GLFW_ICON";
    }

    private static Function user32(String symbol) {
        return NativeLibrary.getInstance("user32").getFunction(symbol, Function.ALT_CONVENTION);
    }

    private static Function objc() {
        return NativeLibrary.getInstance("objc").getFunction("objc_msgSend");
    }

    private static Pointer cls(String name) {
        return NativeLibrary.getInstance("objc").getFunction("objc_getClass").invokePointer(new Object[]{name});
    }

    private static Pointer selector(String name) {
        return NativeLibrary.getInstance("objc").getFunction("sel_registerName").invokePointer(new Object[]{name});
    }

    private static Pointer send(Pointer object, String selector, Object... args) {
        Object[] arguments = new Object[args.length + 2];
        arguments[0] = object;
        arguments[1] = selector(selector);
        System.arraycopy(args, 0, arguments, 2, args.length);
        return objc().invokePointer(arguments);
    }

    private static Pointer nsString(String value) {
        return send(cls("NSString"), "stringWithUTF8String:", value);
    }

    private static String text(Pointer string) {
        return send(string, "UTF8String").getString(0, "UTF-8");
    }

    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }
}
