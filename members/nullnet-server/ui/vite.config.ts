import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      // backend now serves HTTPS with a self-signed cert; secure:false skips cert validation
      '/api': {
        target: 'https://localhost:8080',
        secure: false,
      },
    },
  },
})
