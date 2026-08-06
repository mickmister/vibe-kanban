import React, { useEffect, useState } from 'react';
import { ThemeMode } from 'shared/types';
import type { AppearanceSettings } from '@/shared/lib/themeCustomizations';
import { ThemeProviderContext } from '@/shared/hooks/useTheme';
import { applyAppearanceSettings } from '@/shared/lib/appearance';

type ThemeProviderProps = {
  children: React.ReactNode;
  initialTheme?: ThemeMode;
  appearance?: AppearanceSettings | null;
};

export function ThemeProvider({
  children,
  initialTheme = ThemeMode.SYSTEM,
  appearance,
  ...props
}: ThemeProviderProps) {
  const [theme, setThemeState] = useState<ThemeMode>(initialTheme);

  useEffect(() => {
    setThemeState(initialTheme);
  }, [initialTheme]);

  useEffect(() => {
    const root = window.document.documentElement;

    root.classList.remove('light', 'dark');

    if (theme === ThemeMode.SYSTEM) {
      const systemTheme = window.matchMedia('(prefers-color-scheme: dark)')
        .matches
        ? 'dark'
        : 'light';

      root.classList.add(systemTheme);
      return;
    }

    root.classList.add(theme.toLowerCase());
  }, [theme]);

  useEffect(() => {
    applyAppearanceSettings(appearance);
  }, [appearance]);

  const setTheme = (newTheme: ThemeMode) => {
    setThemeState(newTheme);
  };

  return (
    <ThemeProviderContext.Provider
      {...props}
      value={{
        theme,
        setTheme,
      }}
    >
      {children}
    </ThemeProviderContext.Provider>
  );
}
