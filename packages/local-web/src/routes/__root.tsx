import { useEffect } from 'react';
import { Outlet, createRootRoute } from '@tanstack/react-router';
import { I18nextProvider } from 'react-i18next';
import { usePostHog } from 'posthog-js/react';
import { ThemeMode } from 'shared/types';
import i18n from '@/i18n';
import { useUserSystem } from '@/shared/hooks/useUserSystem';
import { ThemeProvider } from '@/shared/providers/ThemeProvider';
import type { ConfigWithAppearance } from '@/shared/lib/themeCustomizations';
import { useUiPreferencesScratch } from '@/shared/hooks/useUiPreferencesScratch';
import { UserProvider } from '@/shared/providers/remote/UserProvider';
import '@/app/styles/new/index.css';

function RootRouteComponent() {
  const { config, machineId } = useUserSystem();
  const typedConfig = config as ConfigWithAppearance | null;
  const posthog = usePostHog();

  useUiPreferencesScratch();

  useEffect(() => {
    if (!posthog || !machineId) return;

    if (typedConfig?.analytics_enabled) {
      posthog.opt_in_capturing();
      posthog.identify(machineId);
      console.log('[Analytics] Analytics enabled and user identified');
    } else {
      posthog.opt_out_capturing();
      console.log('[Analytics] Analytics disabled by user preference');
    }
  }, [typedConfig?.analytics_enabled, machineId, posthog]);

  return (
    <I18nextProvider i18n={i18n}>
      <ThemeProvider
        initialTheme={typedConfig?.theme || ThemeMode.SYSTEM}
        appearance={typedConfig?.appearance}
      >
        <UserProvider>
          <Outlet />
        </UserProvider>
      </ThemeProvider>
    </I18nextProvider>
  );
}

export const Route = createRootRoute({
  component: RootRouteComponent,
});
