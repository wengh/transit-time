import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from '@tailwindcss/vite';

const THREADING_HEADERS = {
  'Cross-Origin-Opener-Policy': 'same-origin',
  'Cross-Origin-Embedder-Policy': 'require-corp',
};

export default defineConfig({
  root: '.',
  publicDir: 'public',
  plugins: [tailwindcss(), react()],
  build: {
    outDir: 'dist',
  },
  worker: {
    format: 'es',
  },
  server: {
    port: 3000,
    headers: THREADING_HEADERS,
  },
  preview: {
    headers: THREADING_HEADERS,
  },
  appType: 'spa',
});
