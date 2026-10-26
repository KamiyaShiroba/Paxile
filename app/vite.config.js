import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';
export default defineConfig({ root: 'src', publicDir: '../public', clearScreen: false, server: { port: 1420, strictPort: true }, build: { outDir: '../dist', emptyOutDir: true, rollupOptions: { input: { settings: fileURLToPath(new URL('./src/index.html', import.meta.url)), tray: fileURLToPath(new URL('./src/tray.html', import.meta.url)) } } } });
