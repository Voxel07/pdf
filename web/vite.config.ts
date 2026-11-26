import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

export default defineConfig({
  plugins: [svelte()],
  server: {
    port: 5173,
    fs: { allow: ['.', '../calendar/src/lib'] },
    proxy: { '/api': 'http://localhost:8080', '/auth': 'http://localhost:8080' },
  },
  build: { target: 'es2022', chunkSizeWarningLimit: 2000 },
});
