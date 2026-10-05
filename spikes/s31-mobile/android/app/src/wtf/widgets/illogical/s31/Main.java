package wtf.widgets.illogical.s31;

import android.app.Activity;
import android.content.Intent;
import android.os.Bundle;
import android.widget.TextView;

// Starts the service and shows where the daemon's files are. The real app
// would be the web client in a WebView pointed at the daemon.
public class Main extends Activity {
    @Override protected void onCreate(Bundle b) {
        super.onCreate(b);
        if (checkSelfPermission("android.permission.POST_NOTIFICATIONS") != 0)
            requestPermissions(new String[] {"android.permission.POST_NOTIFICATIONS"}, 1);
        // `am start ... --ez wakelock true|false` (S31's screen-off test).
        if (getIntent().hasExtra("wakelock"))
            getSharedPreferences("s31", 0).edit().putBoolean("wakelock", getIntent().getBooleanExtra("wakelock", false)).commit();
        startForegroundService(new Intent(this, Daemon.class));
        TextView t = new TextView(this);
        t.setText("illogicald: " + getApplicationInfo().nativeLibraryDir + "\nstate: " + getFilesDir());
        setContentView(t);
    }
}
