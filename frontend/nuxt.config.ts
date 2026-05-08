// https://nuxt.com/docs/api/configuration/nuxt-config
export default defineNuxtConfig({
  compatibilityDate: '2024-11-01',
  devtools: { enabled: true },
  telemetry: false,

  app: {
    head: {
      link: [
        { rel: 'icon', type: 'image/png', sizes: '128x128', href: '/favicon-128x128.png' },
        { rel: 'icon', type: 'image/x-icon', href: '/favicon.ico' },
        { rel: 'apple-touch-icon', href: '/apple-touch-icon.png' },
      ],
    },
  },

  modules: [
    '@nuxtjs/tailwindcss',
    '@nuxtjs/color-mode',
    '@nuxtjs/i18n',
    '@pinia/nuxt',
  ],

  // Color mode (dark/light theme)
  colorMode: {
    classSuffix: '',
    preference: 'dark',
    fallback: 'dark',
  },

  // i18n
  i18n: {
    locales: [
      { code: 'zh', name: '中文', file: 'zh.json' },
      { code: 'en', name: 'English', file: 'en.json' },
    ],
    defaultLocale: 'zh',
    langDir: '../i18n',
    strategy: 'no_prefix',
    detectBrowserLanguage: {
      useCookie: true,
      cookieKey: 'i18n_locale',
      fallbackLocale: 'zh',
    },
  },

  // API proxy — forwards /api/* to the Rust backend.
  // SSE endpoints (chat, RCA) use Nitro server routes for unbuffered streaming.
  routeRules: {
    '/api/**': {
      proxy: { to: `${process.env.NUXT_BACKEND_URL || 'http://localhost:3080'}/api/**` },
    },
    '/health': {
      proxy: { to: `${process.env.NUXT_BACKEND_URL || 'http://localhost:3080'}/health` },
    },
  },

  // Tailwind
  tailwindcss: {
    cssPath: '~/assets/css/tailwind.css',
    configPath: 'tailwind.config.ts',
  },

  // Runtime config
  runtimeConfig: {
    // Server-only: internal backend URL for SSR API calls
    backendUrl: process.env.NUXT_BACKEND_URL || 'http://localhost:3080',
    public: {
      apiBase: process.env.NUXT_PUBLIC_API_BASE || '',
    },
  },

  // Exclude shadcn-vue barrel files from auto-import to avoid duplicate warnings
  components: {
    dirs: [
      {
        path: '~/components',
        ignore: ['ui/**/index.ts'],
      },
    ],
  },

  // TypeScript
  typescript: {
    strict: true,
  },
})
