import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';

const api = 'http://127.0.0.1:3090';

export default defineConfig({
  plugins: [svelte()],
  server: {
    port: 5174,
    proxy: { '/api': api, '/feeds': api },
  },
});
