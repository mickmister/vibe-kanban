import type { Config } from 'shared/types';

export const APP_FONT_FAMILIES = {
  IBM_PLEX_SANS: 'IBM Plex Sans',
  INTER: 'Inter',
  SPACE_GROTESK: 'Space Grotesk',
  SYSTEM_UI: 'System UI',
} as const;

export type AppFontFamily = keyof typeof APP_FONT_FAMILIES;

export const MONO_FONT_FAMILIES = {
  IBM_PLEX_MONO: 'IBM Plex Mono',
  JET_BRAINS_MONO: 'JetBrains Mono',
  FIRA_CODE: 'Fira Code',
  SOURCE_CODE_PRO: 'Source Code Pro',
} as const;

export type MonospaceFontFamily = keyof typeof MONO_FONT_FAMILIES;

export type ThemePalette = {
  bg_primary?: string | null;
  bg_secondary?: string | null;
  bg_panel?: string | null;
  text_high?: string | null;
  text_normal?: string | null;
  text_low?: string | null;
  brand?: string | null;
};

export type AppearanceSettings = {
  sans_font?: AppFontFamily | null;
  mono_font?: MonospaceFontFamily | null;
  light_palette?: ThemePalette | null;
  dark_palette?: ThemePalette | null;
};

export type ConfigWithAppearance = Config & {
  appearance?: AppearanceSettings | null;
};
