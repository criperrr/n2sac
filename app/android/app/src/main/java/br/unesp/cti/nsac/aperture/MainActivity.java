package br.unesp.cti.nsac.aperture;

import android.content.res.Configuration;
import android.os.Build;
import android.os.Bundle;
import android.webkit.JavascriptInterface;
import android.webkit.WebView;
import androidx.core.content.ContextCompat;
import androidx.core.view.WindowCompat;
import androidx.core.view.WindowInsetsControllerCompat;
import com.getcapacitor.BridgeActivity;

public class MainActivity extends BridgeActivity {

    @Override
    public void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        attachThemeBridge();
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
