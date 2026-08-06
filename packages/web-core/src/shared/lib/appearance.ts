import {
  APP_FONT_FAMILIES,
  MONO_FONT_FAMILIES,
  type AppFontFamily,
  type AppearanceSettings,
  type MonospaceFontFamily,
  type ThemePalette,
} from '@/shared/lib/themeCustomizations';

export const THEME_COLOR_DEFAULTS = {
  light: {
    bg_primary: '#ffffff',
    bg_secondary: '#f2f2f2',
    bg_panel: '#e3e3e3',
    text_high: '#0d0d0d',
    text_normal: '#333333',
    text_low: '#636363',
    brand: '#ea7f2c',
  },
  dark: {
    bg_primary: '#212121',
    bg_secondary: '#1c1c1c',
    bg_panel: '#292929',
    text_high: '#f5f5f5',
    text_normal: '#c4c4c4',
    text_low: '#8f8f8f',
    brand: '#ea7f2c',
  },
} satisfies Record<'light' | 'dark', Record<keyof ThemePalette, string>>;

export const FONT_FAMILY_LABELS = APP_FONT_FAMILIES;

export const MONO_FONT_FAMILY_LABELS = MONO_FONT_FAMILIES;

const FONT_STACKS: Record<AppFontFamily, string> = {
  IBM_PLEX_SANS: '"IBM Plex Sans", "Noto Emoji", sans-serif',
  INTER: '"Inter", "Noto Emoji", sans-serif',
  SPACE_GROTESK: '"Space Grotesk", "Noto Emoji", sans-serif',
  SYSTEM_UI:
    'system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif',
};

const MONO_FONT_STACKS: Record<MonospaceFontFamily, string> = {
  IBM_PLEX_MONO: '"IBM Plex Mono", monospace',
  JET_BRAINS_MONO: '"JetBrains Mono", monospace',
  FIRA_CODE: '"Fira Code", monospace',
  SOURCE_CODE_PRO: '"Source Code Pro", monospace',
};

const paletteKeys: Array<keyof ThemePalette> = [
  'bg_primary',
  'bg_secondary',
  'bg_panel',
  'text_high',
  'text_normal',
  'text_low',
  'brand',
];

function normalizeHexColor(value: string | null | undefined): string | null {
  if (!value) return null;
  const trimmed = value.trim();
  if (/^#[0-9a-f]{6}$/i.test(trimmed)) return trimmed.toLowerCase();
  return null;
}

function hexToHslTriplet(hex: string): string {
  const normalized = hex.replace('#', '');
  const r = parseInt(normalized.slice(0, 2), 16) / 255;
  const g = parseInt(normalized.slice(2, 4), 16) / 255;
  const b = parseInt(normalized.slice(4, 6), 16) / 255;

  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  let h = 0;
  let s = 0;
  const l = (max + min) / 2;

  if (max !== min) {
    const d = max - min;
    s = l > 0.5 ? d / (2 - max - min) : d / (max + min);

    switch (max) {
      case r:
        h = (g - b) / d + (g < b ? 6 : 0);
        break;
      case g:
        h = (b - r) / d + 2;
        break;
      case b:
        h = (r - g) / d + 4;
        break;
    }

    h /= 6;
  }

  return `${Math.round(h * 360)} ${Math.round(s * 100)}% ${Math.round(l * 100)}%`;
}

function adjustHslLightness(hsl: string, delta: number): string {
  const [h = '0', s = '0%', l = '0%'] = hsl.split(' ');
  const nextL = Math.max(0, Math.min(100, parseFloat(l) + delta));
  return `${h} ${s} ${Math.round(nextL)}%`;
}

function setCssVar(name: string, value: string | null | undefined) {
  if (value) {
    document.documentElement.style.setProperty(name, value);
  } else {
    document.documentElement.style.removeProperty(name);
  }
}

export function getThemePaletteValue(
  mode: 'light' | 'dark',
  palette: ThemePalette | null | undefined,
  key: keyof ThemePalette
): string {
  return normalizeHexColor(palette?.[key]) ?? THEME_COLOR_DEFAULTS[mode][key];
}

export function applyAppearanceSettings(
  appearance: AppearanceSettings | null | undefined
) {
  const sansFont = appearance?.sans_font ?? 'IBM_PLEX_SANS';
  const monoFont = appearance?.mono_font ?? 'IBM_PLEX_MONO';

  setCssVar('--app-font-sans', FONT_STACKS[sansFont]);
  setCssVar('--app-font-mono', MONO_FONT_STACKS[monoFont]);

  (['light', 'dark'] as const).forEach((mode) => {
    const palette =
      mode === 'light' ? appearance?.light_palette : appearance?.dark_palette;

    paletteKeys.forEach((key) => {
      const hex = normalizeHexColor(palette?.[key]);
      setCssVar(
        `--theme-${mode}-${key.replace(/_/g, '-')}`,
        hex ? hexToHslTriplet(hex) : null
      );
    });

    const brandHex = normalizeHexColor(palette?.brand);
    if (brandHex) {
      const brand = hexToHslTriplet(brandHex);
      setCssVar(`--theme-${mode}-brand-hover`, adjustHslLightness(brand, 8));
      setCssVar(
        `--theme-${mode}-brand-secondary`,
        adjustHslLightness(brand, -17)
      );
    } else {
      setCssVar(`--theme-${mode}-brand-hover`, null);
      setCssVar(`--theme-${mode}-brand-secondary`, null);
    }
  });
}
