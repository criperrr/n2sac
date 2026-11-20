import type { CapacitorConfig } from '@capacitor/cli';

const config: CapacitorConfig = {
  appId: 'com.cti.n2sac',
  appName: 'n²sac',
  webDir: 'dist',
  server: {
    url: 'https://notebook-server.tailddd6bf.ts.net',
    androidScheme: 'https',
    cleartext: false,
  },
  plugins: {
    BackgroundRunner: {
      label: 'com.cti.n2sac.update-check',
      src: 'runners/update-check.js',
      event: 'checkForUpdates',
      repeat: true,
      interval: 60,
      autoStart: true,
    },
    SplashScreen: {
      launchShowDuration: 600,
      backgroundColor: '#0D0D0D',
      showSpinner: false,
    },
  },
};

export default config;
