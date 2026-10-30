package org.blockops.bridge;

import com.sun.jna.Native;
import com.sun.jna.platform.win32.Advapi32;
import com.sun.jna.platform.win32.Advapi32Util;
import com.sun.jna.platform.win32.Kernel32;
import com.sun.jna.platform.win32.WinBase;
import com.sun.jna.platform.win32.WinDef;
import com.sun.jna.platform.win32.WinNT;
import com.sun.jna.ptr.IntByReference;
import com.sun.jna.ptr.PointerByReference;
import com.sun.jna.win32.StdCallLibrary;
import com.sun.jna.win32.W32APIOptions;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;

/**
 * Windows descriptor store. The file is created with an explicit owner and a protected
 * DACL that allows only the current user, so no secret is ever readable through inherited
 * permissions. It is written under a temporary name and then renamed, which keeps its
 * security descriptor. Minecraft ships JNA, so nothing is bundled with the mod.
 */
public final class WindowsDescriptorFile implements DiscoveryPublisher.Store {
    private interface Sddl extends StdCallLibrary {
        Sddl INSTANCE = Native.load("advapi32", Sddl.class, W32APIOptions.UNICODE_OPTIONS);
        boolean ConvertStringSecurityDescriptorToSecurityDescriptor(
            String descriptor, int revision, PointerByReference result, IntByReference size);
    }

    private final Path target;
    private final Path staging;
    private final String sddl;

    public WindowsDescriptorFile(Path directory, long pid) throws IOException {
        Files.createDirectories(directory);
        DiscoveryPublisher.removeStale(directory, System.currentTimeMillis());
        this.target = directory.resolve("minecraft-" + pid + ".json");
        this.staging = directory.resolve("minecraft-" + pid + ".tmp");
        String sid = currentUserSid();
        this.sddl = "O:" + sid + "D:P(A;;FA;;;" + sid + ")";
    }

    /** %LOCALAPPDATA%\BlockOps\discovery, shared with the desktop application. */
    public static Path defaultDirectory() throws IOException {
        String base = System.getenv("LOCALAPPDATA");
        if (base == null || base.isBlank()) throw new IOException("LOCALAPPDATA is unavailable");
        return Path.of(base, "BlockOps", "discovery");
    }

    private static String currentUserSid() throws IOException {
        WinNT.HANDLEByReference token = new WinNT.HANDLEByReference();
        if (!Advapi32.INSTANCE.OpenProcessToken(Kernel32.INSTANCE.GetCurrentProcess(), WinNT.TOKEN_QUERY, token)) {
            throw new IOException("Cannot inspect current user");
        }
        try {
            IntByReference size = new IntByReference();
            Advapi32.INSTANCE.GetTokenInformation(token.getValue(), WinNT.TOKEN_INFORMATION_CLASS.TokenUser, null, 0, size);
            if (size.getValue() <= 0 || size.getValue() > 65536) throw new IOException("Cannot inspect current user");
            WinNT.TOKEN_USER user = new WinNT.TOKEN_USER(size.getValue());
            if (!Advapi32.INSTANCE.GetTokenInformation(token.getValue(), WinNT.TOKEN_INFORMATION_CLASS.TokenUser,
                    user, size.getValue(), size)) {
                throw new IOException("Cannot inspect current user");
            }
            String sid = Advapi32Util.convertSidToStringSid(user.User.Sid);
            if (sid == null || !sid.matches("S-1(-[0-9]+)+")) throw new IOException("Cannot inspect current user");
            return sid;
        } finally {
            Kernel32.INSTANCE.CloseHandle(token.getValue());
        }
    }

    @Override public void publish(byte[] descriptor) throws IOException {
        Kernel32.INSTANCE.DeleteFile(staging.toString());
        PointerByReference security = new PointerByReference();
        if (!Sddl.INSTANCE.ConvertStringSecurityDescriptorToSecurityDescriptor(sddl, 1, security, null)) {
            throw new IOException("Cannot build private descriptor permissions");
        }
        WinNT.HANDLE file;
        try {
            WinBase.SECURITY_ATTRIBUTES attributes = new WinBase.SECURITY_ATTRIBUTES();
            attributes.lpSecurityDescriptor = security.getValue();
            attributes.bInheritHandle = false;
            file = Kernel32.INSTANCE.CreateFile(staging.toString(), WinNT.GENERIC_WRITE, 0, attributes,
                WinNT.CREATE_NEW, WinNT.FILE_ATTRIBUTE_NORMAL, null);
        } finally {
            Kernel32.INSTANCE.LocalFree(security.getValue());
        }
        if (file == null || WinBase.INVALID_HANDLE_VALUE.equals(file)) {
            throw new IOException("Cannot create private descriptor");
        }
        boolean written;
        try {
            IntByReference count = new IntByReference();
            written = Kernel32.INSTANCE.WriteFile(file, descriptor, descriptor.length, count, null)
                && count.getValue() == descriptor.length && Kernel32.INSTANCE.FlushFileBuffers(file);
        } finally {
            Kernel32.INSTANCE.CloseHandle(file);
        }
        // A reader holding the old descriptor blocks the rename; the caller retries later.
        if (!written || !Kernel32.INSTANCE.MoveFileEx(staging.toString(), target.toString(),
                new WinDef.DWORD(WinBase.MOVEFILE_REPLACE_EXISTING | WinBase.MOVEFILE_WRITE_THROUGH))) {
            Kernel32.INSTANCE.DeleteFile(staging.toString());
            throw new IOException("Cannot publish private descriptor");
        }
    }

    @Override public void remove() {
        Kernel32.INSTANCE.DeleteFile(target.toString());
        Kernel32.INSTANCE.DeleteFile(staging.toString());
    }
}
