package wtf.widgets.illogical.s31;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.os.IBinder;
import android.os.PowerManager;
import java.io.File;
import java.util.Map;

// A foreground service that runs illogicald from the native lib dir (the one
// place an app targeting API 29+ may exec files from) and restarts it if it
// exits. Everything else (panes, control) is the daemon's own business.
public class Daemon extends Service {
    private Process proc;
    private PowerManager.WakeLock lock;

    @Override public int onStartCommand(Intent i, int flags, int id) {
        NotificationManager nm = getSystemService(NotificationManager.class);
        nm.createNotificationChannel(new NotificationChannel("d", "Terminals", NotificationManager.IMPORTANCE_LOW));
        Notification n = new Notification.Builder(this, "d")
            .setContentTitle("illogical").setContentText("Terminals are running")
            .setSmallIcon(android.R.drawable.ic_menu_manage).setOngoing(true).build();
        startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE);
        if (getSharedPreferences("s31", 0).getBoolean("wakelock", false) && lock == null) {
            lock = getSystemService(PowerManager.class).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "illogical:d");
            lock.acquire();
        }
        if (proc == null) new Thread(this::supervise).start();
        return START_STICKY;
    }

    private void supervise() {
        String lib = getApplicationInfo().nativeLibraryDir;
        File home = getFilesDir();
        while (true) {
            try {
                ProcessBuilder pb = new ProcessBuilder(lib + "/libillogicald.so", "--no-update-check");
                Map<String, String> env = pb.environment();
                env.put("HOME", home.getPath());
                env.put("TMPDIR", getCacheDir().getPath());
                env.put("SHELL", "/system/bin/sh");
                env.put("PATH", lib + "/bin:" + home + "/bin:/system/bin");
                env.put("ILLOGICAL_LIB", lib);
                pb.redirectErrorStream(true).redirectOutput(ProcessBuilder.Redirect.appendTo(new File(home, "d.log")));
                proc = pb.start();
                proc.waitFor();
            } catch (Exception e) {
                android.util.Log.e("illogical", "daemon", e);
            }
            try { Thread.sleep(2000); } catch (InterruptedException e) { return; }
        }
    }

    @Override public IBinder onBind(Intent i) { return null; }
}
