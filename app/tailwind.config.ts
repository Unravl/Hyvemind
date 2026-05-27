import type { Config } from "tailwindcss";

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      fontFamily: {
        sans: ["Inter", "ui-sans-serif", "system-ui", "sans-serif"],
        mono: ['"JetBrains Mono"', "ui-monospace", "SFMono-Regular", "monospace"],
      },
      colors: {
        ink: {
          950: "#07080C",
          900: "#0B0D12",
          850: "#0F1219",
          800: "#13161F",
          700: "#1A1E2A",
          600: "#222837",
          500: "#2A3142",
          400: "#3A4254",
        },
        line: {
          DEFAULT: "#1F2433",
          soft: "#171B26",
          strong: "#2C3346",
        },
        honey: {
          50: "#FFF8E1",
          100: "#FFEEB0",
          200: "#FFE07A",
          300: "#FFD24A",
          400: "#FAC322",
          500: "#F5B919",
          600: "#D99A0E",
          700: "#A6730A",
          800: "#6E4C06",
          900: "#3E2B03",
        },
        muted: "#8B92A5",
        dim: "#5A6072",
      },
      boxShadow: {
        glow: "0 0 0 1px rgba(245,185,25,.35), 0 8px 32px -8px rgba(245,185,25,.35)",
        panel:
          "0 1px 0 rgba(255,255,255,.02) inset, 0 0 0 1px rgba(255,255,255,.02)",
      },
    },
  },
  plugins: [],
} satisfies Config;
