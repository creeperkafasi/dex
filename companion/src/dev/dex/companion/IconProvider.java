package dev.dex.companion;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.database.Cursor;
import android.graphics.Bitmap;
import android.graphics.Canvas;
import android.graphics.drawable.Drawable;
import android.net.Uri;
import android.os.Binder;
import android.os.ParcelFileDescriptor;
import android.os.Process;

import java.io.FileNotFoundException;
import java.io.IOException;
import java.io.OutputStream;

/**
 * Serves an app's launcher icon as a PNG:
 *
 *   adb exec-out content read --uri content://dev.dex.companion.icons/<package>[?size=N]
 *
 * or every launcher icon at once, as repeated records of
 * [u16 package length][package, UTF-8][u32 PNG length][PNG] (big-endian):
 *
 *   adb exec-out content read --uri content://dev.dex.companion.icons/_all[?size=N]
 *
 * Only adb (shell or root) may read it; other apps on the phone are refused.
 */
public class IconProvider extends ContentProvider {
    private static final int DEFAULT_SIZE = 128;
    private static final int MAX_SIZE = 512;

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public ParcelFileDescriptor openFile(Uri uri, String mode) throws FileNotFoundException {
        int caller = Binder.getCallingUid();
        if (caller != Process.SHELL_UID && caller != Process.ROOT_UID) {
            throw new SecurityException("dex companion only answers adb");
        }
        String pkg = uri.getLastPathSegment();
        if (pkg == null) {
            throw new FileNotFoundException("no package in " + uri);
        }
        int size = DEFAULT_SIZE;
        String sizeParam = uri.getQueryParameter("size");
        if (sizeParam != null) {
            try {
                size = Math.max(16, Math.min(MAX_SIZE, Integer.parseInt(sizeParam)));
            } catch (NumberFormatException ignored) {
                // keep the default
            }
        }

        final int iconSize = size;
        if (pkg.equals("_all")) {
            return stream(out -> writeAll(new java.io.DataOutputStream(out), iconSize));
        }
        Bitmap bitmap = render(loadIcon(pkg), size);
        return stream(out -> bitmap.compress(Bitmap.CompressFormat.PNG, 100, out));
    }

    private interface Writer {
        void write(OutputStream out) throws IOException;
    }

    /** Runs `writer` on a thread, feeding the returned read end of a pipe. */
    private static ParcelFileDescriptor stream(Writer writer) throws FileNotFoundException {
        ParcelFileDescriptor[] pipe;
        try {
            pipe = ParcelFileDescriptor.createPipe();
        } catch (IOException e) {
            throw new FileNotFoundException(e.toString());
        }
        ParcelFileDescriptor writeEnd = pipe[1];
        new Thread(() -> {
            try (OutputStream out = new java.io.BufferedOutputStream(
                    new ParcelFileDescriptor.AutoCloseOutputStream(writeEnd), 1 << 16)) {
                writer.write(out);
            } catch (IOException ignored) {
                // reader went away
            }
        }, "dex-icon").start();
        return pipe[0];
    }

    private void writeAll(java.io.DataOutputStream out, int size) throws IOException {
        PackageManager pm = getContext().getPackageManager();
        Intent launch = new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER);
        java.util.Set<String> seen = new java.util.HashSet<>();
        java.io.ByteArrayOutputStream png = new java.io.ByteArrayOutputStream();
        for (ResolveInfo info : pm.queryIntentActivities(launch, 0)) {
            String pkg = info.activityInfo.packageName;
            if (!seen.add(pkg)) {
                continue;
            }
            png.reset();
            render(info.loadIcon(pm), size).compress(Bitmap.CompressFormat.PNG, 100, png);
            byte[] name = pkg.getBytes(java.nio.charset.StandardCharsets.UTF_8);
            out.writeShort(name.length);
            out.write(name);
            out.writeInt(png.size());
            png.writeTo(out);
        }
    }

    /** The icon the launcher shows, falling back to the application icon. */
    private Drawable loadIcon(String pkg) throws FileNotFoundException {
        PackageManager pm = getContext().getPackageManager();
        Intent launch = new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER).setPackage(pkg);
        for (ResolveInfo info : pm.queryIntentActivities(launch, 0)) {
            return info.loadIcon(pm);
        }
        try {
            return pm.getApplicationIcon(pkg);
        } catch (PackageManager.NameNotFoundException e) {
            throw new FileNotFoundException("no such package: " + pkg);
        }
    }

    private static Bitmap render(Drawable icon, int size) {
        Bitmap bitmap = Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888);
        Canvas canvas = new Canvas(bitmap);
        icon.setBounds(0, 0, size, size);
        icon.draw(canvas);
        return bitmap;
    }

    // Not a data provider: everything else is unsupported.

    @Override
    public Cursor query(Uri uri, String[] projection, String selection, String[] args, String sortOrder) {
        return null;
    }

    @Override
    public String getType(Uri uri) {
        return "image/png";
    }

    @Override
    public Uri insert(Uri uri, ContentValues values) {
        throw new UnsupportedOperationException();
    }

    @Override
    public int delete(Uri uri, String selection, String[] args) {
        throw new UnsupportedOperationException();
    }

    @Override
    public int update(Uri uri, ContentValues values, String selection, String[] args) {
        throw new UnsupportedOperationException();
    }
}
