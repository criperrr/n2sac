package br.unesp.cti.nsac.aperture;

import android.content.Intent;
import android.content.pm.PackageInfo;
import android.content.res.Configuration;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.provider.Settings;
import android.webkit.JavascriptInterface;
import android.webkit.WebView;
import androidx.activity.BackEventCompat;
import androidx.activity.OnBackPressedCallback;
import androidx.annotation.NonNull;
import androidx.core.content.ContextCompat;
import androidx.core.content.FileProvider;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsControllerCompat;
import com.getcapacitor.BridgeActivity;
import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;

public class MainActivity extends BridgeActivity {

    /**
     * Predictive back bridge. Enabled by the web app only while its back stack is non-empty
     * (open popup, pushed page, non-start tab...). While disabled, the system handles back and
     * plays its own back-to-home animation. While enabled, every gesture phase is forwarded to
     * the WebView as an `nsac:back` event so the page can run the in-app preview animation.
     */
    private OnBackPressedCallback predictiveBackCallback;

    @Override
    public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        attachPredictiveBackBridge();
        attachThemeBridge();
        attachApkUpdaterBridge();
        attachExternalOpenBridge();
        syncAospDynamicColorsToWebView();
    }

    @Override
    public void onResume() {
        super.onResume();
        syncAospDynamicColorsToWebView();
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        syncAospDynamicColorsToWebView();
    }

    private void attachPredictiveBackBridge() {
        predictiveBackCallback = new OnBackPressedCallback(false) {
            @Override
            public void handleOnBackStarted(@NonNull BackEventCompat backEvent) {
                dispatchBackEvent("start", backEvent);
            }

            @Override
            public void handleOnBackProgressed(@NonNull BackEventCompat backEvent) {
                dispatchBackEvent("progress", backEvent);
            }

            @Override
            public void handleOnBackCancelled() {
                dispatchBackEvent("cancel", null);
            }

            @Override
            public void handleOnBackPressed() {
                dispatchBackEvent("commit", null);
            }
        };
        // Registered after the bridge plugins, so it takes precedence whenever it is enabled.
        getOnBackPressedDispatcher().addCallback(this, predictiveBackCallback);

        if (this.bridge == null) return;
        WebView webView = this.bridge.getWebView();
        if (webView == null) return;
        webView.addJavascriptInterface(new Object() {
            @JavascriptInterface
            public void setEnabled(final boolean enabled) {
                runOnUiThread(() -> predictiveBackCallback.setEnabled(enabled));
            }
        }, "NsacAndroidBack");
    }

    private void dispatchBackEvent(String phase, BackEventCompat backEvent) {
        if (this.bridge == null) return;
        WebView webView = this.bridge.getWebView();
        if (webView == null) return;

        float density = getResources().getDisplayMetrics().density;
        String detail = backEvent == null
            ? String.format(java.util.Locale.ROOT, "{\"phase\":\"%s\",\"density\":%f}", phase, density)
            : String.format(
                java.util.Locale.ROOT,
                "{\"phase\":\"%s\",\"progress\":%f,\"touchX\":%f,\"touchY\":%f,\"edge\":%d,\"density\":%f}",
                phase,
                backEvent.getProgress(),
                backEvent.getTouchX(),
                backEvent.getTouchY(),
                backEvent.getSwipeEdge(),
                density
            );
        webView.evaluateJavascript(
            "window.dispatchEvent(new CustomEvent('nsac:back',{detail:" + detail + "}));",
            null
        );
    }

    private void attachThemeBridge() {
        if (this.bridge == null) return;
        WebView webView = this.bridge.getWebView();
        if (webView == null) return;
        webView.addJavascriptInterface(new Object() {
            @JavascriptInterface
            public void setLightSystemBars(final boolean isLight) {
                runOnUiThread(() -> {
                    WindowInsetsControllerCompat controller =
                        WindowCompat.getInsetsController(getWindow(), getWindow().getDecorView());
                    if (controller != null) {
                        controller.setAppearanceLightStatusBars(isLight);
                        controller.setAppearanceLightNavigationBars(isLight);
                    }
                });
            }
        }, "NsacAndroidTheme");
    }

    /**
     * `NsacAndroidOpen.openExternal(url)`: opens an http(s) URL in the system browser. Capacitor
     * keeps same-host URLs inside the WebView, which can't show PDFs (e.g. the ficha to print).
     */
    private void attachExternalOpenBridge() {
        if (this.bridge == null) return;
        WebView webView = this.bridge.getWebView();
        if (webView == null) return;
        webView.addJavascriptInterface(new Object() {
            @JavascriptInterface
            public boolean openExternal(final String rawUrl) {
                final Uri uri;
                try {
                    uri = Uri.parse(rawUrl);
                } catch (Exception e) {
                    return false;
                }
                String scheme = uri.getScheme();
                if (scheme == null || !(scheme.equals("https") || scheme.equals("http"))) return false;
                runOnUiThread(() -> {
                    try {
                        Intent intent = new Intent(Intent.ACTION_VIEW, uri);
                        intent.addCategory(Intent.CATEGORY_BROWSABLE);
                        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                        startActivity(intent);
                    } catch (Exception ignored) {
                        // No browser available: nothing to open it with.
                    }
                });
                return true;
            }
        }, "NsacAndroidOpen");
    }

    private void attachApkUpdaterBridge() {
        if (this.bridge == null) return;
        WebView webView = this.bridge.getWebView();
        if (webView == null) return;

        webView.addJavascriptInterface(new Object() {
            @JavascriptInterface
            public String getInstalledVersion() {
                try {
                    PackageInfo info = getPackageManager().getPackageInfo(getPackageName(), 0);
                    return info.versionName != null ? info.versionName : "1.0.0";
                } catch (Exception e) {
                    return "1.0.0";
                }
            }

            @JavascriptInterface
            public void downloadApk(final String apkUrl) {
                new Thread(() -> {
                    HttpURLConnection conn = null;
                    try {
                        dispatchApkDownloadEvent("downloading", 0, null);
                        URL url = new URL(apkUrl);
                        conn = (HttpURLConnection) url.openConnection();
                        conn.setConnectTimeout(15000);
                        conn.setReadTimeout(30000);
                        conn.setInstanceFollowRedirects(true);
                        conn.connect();

                        int status = conn.getResponseCode();
                        if (status < 200 || status >= 300) {
                            throw new Exception("HTTP " + status);
                        }

                        long total = conn.getContentLengthLong();
                        File outFile = new File(getCacheDir(), "n2sac-update.apk");
                        try (InputStream in = conn.getInputStream();
                             FileOutputStream out = new FileOutputStream(outFile)) {
                            byte[] buf = new byte[16384];
                            long downloaded = 0;
                            int lastPct = -1;
                            int read;
                            while ((read = in.read(buf)) != -1) {
                                out.write(buf, 0, read);
                                downloaded += read;
                                if (total > 0) {
                                    int pct = (int) Math.min(99, (downloaded * 100L) / total);
                                    if (pct != lastPct) {
                                        lastPct = pct;
                                        dispatchApkDownloadEvent("downloading", pct, null);
                                    }
                                }
                            }
                            out.flush();
                        }
                        dispatchApkDownloadEvent("done", 100, null);
                    } catch (Exception e) {
                        dispatchApkDownloadEvent("error", 0, e.getMessage());
                    } finally {
                        if (conn != null) conn.disconnect();
                    }
                }).start();
            }

            @JavascriptInterface
            public void installDownloadedApk() {
                runOnUiThread(() -> {
                    try {
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O
                                && !getPackageManager().canRequestPackageInstalls()) {
                            Intent permIntent = new Intent(
                                Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                                Uri.parse("package:" + getPackageName())
                            );
                            permIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                            startActivity(permIntent);
                            return;
                        }
                        File apkFile = new File(getCacheDir(), "n2sac-update.apk");
                        if (!apkFile.exists()) return;
                        Uri uri = FileProvider.getUriForFile(
                            MainActivity.this,
                            getPackageName() + ".fileprovider",
                            apkFile
                        );
                        Intent intent = new Intent(Intent.ACTION_VIEW);
                        intent.setDataAndType(uri, "application/vnd.android.package-archive");
                        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_ACTIVITY_NEW_TASK);
                        startActivity(intent);
                    } catch (Exception e) {
                        dispatchApkDownloadEvent("error", 100, "Falha ao abrir instalador: " + e.getMessage());
                    }
                });
            }
        }, "NsacAndroidUpdater");
    }

    private void dispatchApkDownloadEvent(String state, int progress, String error) {
        if (this.bridge == null) return;
        WebView webView = this.bridge.getWebView();
        if (webView == null) return;
        String safeError = error == null ? "null" : ("\"" + error.replace("\\", "\\\\").replace("\"", "\\\"") + "\"");
        String detail = String.format(
            java.util.Locale.ROOT,
            "{\"state\":\"%s\",\"progress\":%d,\"error\":%s}",
            state,
            progress,
            safeError
        );
        runOnUiThread(() ->
            webView.evaluateJavascript(
                "window.dispatchEvent(new CustomEvent('nsac:apk-download',{detail:" + detail + "}));",
                null
            )
        );
    }

    private String colorIntToHex(int color) {
        return String.format("#%06X", 0xFFFFFF & color);
    }

    private void syncAospDynamicColorsToWebView() {
        if (this.bridge == null) return;
        WebView webView = this.bridge.getWebView();
        if (webView == null) return;

        int nightMask = getResources().getConfiguration().uiMode & Configuration.UI_MODE_NIGHT_MASK;
        boolean isSystemDark = (nightMask == Configuration.UI_MODE_NIGHT_YES);

        String seedHex = "#10A37F";
        String secondaryHex = "#0D0D0D";
        String tertiaryHex = "#F5F5F5";

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            int accent1 = ContextCompat.getColor(this, android.R.color.system_accent1_500);
            int accent2 = ContextCompat.getColor(this, android.R.color.system_accent2_500);
            int accent3 = ContextCompat.getColor(this, android.R.color.system_accent3_500);
            seedHex = colorIntToHex(accent1);
            secondaryHex = colorIntToHex(accent2);
            tertiaryHex = colorIntToHex(accent3);
        }

        final String payloadJson = String.format(
            "{\"seedHex\":\"%s\",\"secondaryHex\":\"%s\",\"tertiaryHex\":\"%s\",\"isSystemDark\":%b,\"source\":\"android-aosp\"}",
            seedHex, secondaryHex, tertiaryHex, isSystemDark
        );

        final String js = "(function(){"
            + "window.__NSAC_ANDROID_DYNAMIC_COLOR__ = " + payloadJson + ";"
            + "window.dispatchEvent(new CustomEvent('nsac:android-dynamic-color', { detail: " + payloadJson + " }));"
            + "})();";

        webView.post(() -> webView.evaluateJavascript(js, null));
    }
}
