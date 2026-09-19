import { defineConfig } from 'vitepress'

const SITE_URL = 'https://crabboss.vercel.app'
const SITE_DESC =
  'Gapless playout, true-PFL cueing, and live streaming (Icecast + Shoutcast) — free and open source.'

export default defineConfig({
  title: 'CrabBoss',
  description: SITE_DESC,
  head: [
    ['meta', { name: 'theme-color', content: '#16171a' }],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:site_name', content: 'CrabBoss' }],
    ['meta', { property: 'og:title', content: 'CrabBoss — Radio automation software' }],
    ['meta', { property: 'og:description', content: SITE_DESC }],
    ['meta', { property: 'og:url', content: `${SITE_URL}/` }],
    ['meta', { property: 'og:image', content: `${SITE_URL}/og-image.png` }],
    ['meta', { name: 'twitter:card', content: 'summary_large_image' }],
    ['meta', { name: 'twitter:title', content: 'CrabBoss — Radio automation software' }],
    ['meta', { name: 'twitter:description', content: SITE_DESC }],
    ['meta', { name: 'twitter:image', content: `${SITE_URL}/og-image.png` }],
  ],
  themeConfig: {
    nav: [
      { text: 'Guide', link: '/guide/getting-started' },
      {
        text: 'Roadmap',
        link: 'https://github.com/sonyarianto/crabboss/blob/main/ROADMAP.md',
      },
      {
        text: 'Sponsor',
        items: [
          { text: 'GitHub Sponsors', link: 'https://github.com/sponsors/sonyarianto' },
          { text: 'Buy Me a Coffee', link: 'https://buymeacoffee.com/sonyarianto' },
          { text: 'Ko-fi', link: 'https://ko-fi.com/sonyarianto' },
        ],
      },
    ],
    sidebar: [
      {
        text: 'Guide',
        items: [
          { text: 'Getting Started', link: '/guide/getting-started' },
          { text: 'User Manual', link: '/guide/user-manual' },
        ],
      },
      {
        text: 'Project',
        items: [
          {
            text: 'Architecture',
            link: 'https://github.com/sonyarianto/crabboss/blob/main/docs/architecture.md',
          },
          { text: 'GitHub', link: 'https://github.com/sonyarianto/crabboss' },
          { text: 'Sponsor', link: 'https://buymeacoffee.com/sonyarianto' },
          { text: 'Ko-fi', link: 'https://ko-fi.com/sonyarianto' },
        ],
      },
    ],
    socialLinks: [{ icon: 'github', link: 'https://github.com/sonyarianto/crabboss' }],
    search: { provider: 'local' },
    footer: {
      message: 'MIT Licensed',
      copyright: 'Copyright © CrabBoss contributors',
    },
  },
})
