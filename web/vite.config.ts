import path from "path"
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    rollupOptions: {
      output: {
        // Separate content-hashed vendor chunks preserve unchanged libraries
        // across app-only OTA updates. Vite uses Rollup's manualChunks API.
        manualChunks(id: string) {
          if (/[\\/]node_modules[\\/](react|react-dom|react-router|react-router-dom)[\\/]/.test(id)) return 'vendor-react'
          if (/[\\/]node_modules[\\/]@xterm[\\/]/.test(id)) return 'vendor-term'
          if (/[\\/]node_modules[\\/]lucide-react[\\/]/.test(id)) return 'vendor-icons'
        },
      },
    },
    // Keep xterm out of default modulepreloads until the Terminal route loads.
    modulePreload: {
      resolveDependencies: (_filename, deps) =>
        deps.filter((d) => !d.includes('vendor-term')),
    },
  },
  server: {
    allowedHosts: true,
    proxy: {
      // Backend API target. Defaults to the local Rust server on :8788;
      // set DASHUSB_API (e.g. http://dashusb.local) to develop the
      // UI against a live Pi without running the backend locally.
      '/api': process.env.DASHUSB_API || 'http://localhost:8788',
      '/Recordings': process.env.DASHUSB_API || 'http://localhost:8788',
    },
  },
})
