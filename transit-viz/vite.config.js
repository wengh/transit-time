import { defineConfig } from 'vite';

export default defineConfig({
  root: '.',
  publicDir: 'public',
  build: {
    outDir: 'dist',
  },
  server: {
    port: 3000,
  },
  // SPA fallback: serve index.html for city routes like /chicago, /chapel_hill
  appType: 'spa',
});
