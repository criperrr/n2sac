import type { CapacitorConfig } from '@capacitor/cli';

const config: CapacitorConfig = {
  appId: 'com.cti.n2sac',
  appName: 'n²sac',
  webDir: 'dist',
  server: {
    androidScheme: 'http',
    cleartext: true,
  },
  plugins: {
    SplashScreen: {
      launchShowDuration: 600,
      backgroundColor: '#0D0D0D',
      showSpinner: false,
    },
  },
};

export default config;
