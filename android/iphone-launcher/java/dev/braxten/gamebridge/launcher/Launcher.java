package dev.braxten.gamebridge.launcher;

import android.app.Activity;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.os.Bundle;
import android.widget.Toast;

/**
 * An iPhone game's own app: opens the game in Game Bridge, which runs it from
 * its folder (named in this app's manifest), so its saves stay in Game Bridge.
 */
public class Launcher extends Activity {
    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        try {
            ApplicationInfo info = getPackageManager().getApplicationInfo(getPackageName(), PackageManager.GET_META_DATA);
            Intent open = new Intent()
                .setClassName("dev.braxten.gamebridge", "dev.braxten.gamebridge.OpenIphoneGame")
                .putExtra("app", info.metaData.getString("dev.braxten.gamebridge.game"))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
            startActivity(open);
        } catch (Exception e) {
            Toast.makeText(this, "This game opens in Game Bridge, which isn't installed.", Toast.LENGTH_LONG).show();
        }
        finish();
    }
}
