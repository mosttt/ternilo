import * as React from "react";
import {
  AlertCircle,
  CheckCircle2,
  Info,
  LoaderCircle,
  RadioTower,
  X,
} from "lucide-react";
import {
  WorkbenchProvider,
  storage,
  useWorkbench,
} from "@/state/workbench";
import { useSessionRuntime, type SessionRuntime } from "@/state/use-session-runtime";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, Input, Label } from "@/components/ui/field";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ConversationColumn } from "@/components/workbench/conversation-column";
import { AppFrame } from "@/components/layout/app-frame";
import { WorkspacePanel } from "@/components/workbench/workspace-panel";
import { WorkspacePanelContext, useWorkspacePanel } from "@/components/workbench/workspace-panel-context";
import {
  DetailsPanel,
  type DetailsSelection,
} from "@/components/workbench/details-panel";
import { DirectoryPicker } from "@/components/workbench/directory-picker";
import { AccountEmailPage } from "@/components/workbench/account-email-page";
import { ServerLogin } from "@/components/workbench/server-login";
import { Sidebar } from "@/components/workbench/sidebar";
import { SettingsDialog, UserSettingsPage } from "@/components/settings/settings-dialog";
import { useWorkbenchLayout } from "@/hooks/use-workbench-layout";
import { applyThemePreference, type ThemePreference } from "@/domain/theme";
import { cn } from "@/lib/utils";
import { LocaleProvider, useLocale, useTranslate } from "@/i18n/provider";
import { registerPwa } from "@/pwa";
import type { SettingsSection } from "@/types";
import { AdminShell } from "@/components/admin/admin-shell";
import { ModelDeviceApproval } from "@/components/models/model-device-approval";
import { ModelAccessShell } from "@/components/models/model-service-pages";
import { FilesPage } from "@/components/files/files-page";
import { navigate, usePathname } from "./navigation";

function applyStoredTheme() {
  const theme = (localStorage.getItem(storage.theme) ?? "system") as ThemePreference;
  applyThemePreference(theme);
}

function ToastRegion() {
  const { toasts, dismissToast } = useWorkbench();
  const t = useTranslate("app");
  return (
    <div
      className="pointer-events-none fixed inset-x-4 bottom-[max(1rem,env(safe-area-inset-bottom))] z-[90] ml-auto flex w-[min(380px,calc(100vw-2rem))] flex-col gap-2"
      role="region"
      aria-label={t("notification.region")}
      aria-live="polite"
      aria-relevant="additions"
    >
      {toasts.map((toast) => {
        const Icon =
          toast.kind === "error"
            ? AlertCircle
            : toast.kind === "info"
              ? Info
              : CheckCircle2;
        return (
          <div
            className={cn(
              "pointer-events-auto flex items-start gap-3 rounded-xl border bg-popover p-3 text-sm shadow-2xl",
              toast.kind === "error" && "border-destructive/35",
            )}
            key={toast.id}
            role={toast.kind === "error" ? "alert" : "status"}
          >
            <Icon
              className={cn(
                "mt-0.5 size-4 shrink-0 text-success",
                toast.kind === "error" && "text-destructive",
                toast.kind === "info" && "text-primary",
              )}
            />
            <span className="min-w-0 flex-1 leading-relaxed">
              {toast.message}
            </span>
            <button
              type="button"
              className="-m-2 grid size-10 shrink-0 place-items-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              aria-label={t("notification.dismiss")}
              onClick={() => dismissToast(toast.id)}
            >
              <X className="size-4" />
            </button>
          </div>
        );
      })}
    </div>
  );
}

function RemoteLogin() {
  const { authRequired, remote, error: connectionError, login } = useWorkbench();
  const t = useTranslate("app");
  const [token, setToken] = React.useState("");
  const [remember, setRemember] = React.useState(false);
  const [error, setError] = React.useState("");
  const [loading, setLoading] = React.useState(false);
  const submit = async () => {
    if (!token.trim()) return;
    setLoading(true);
    setError("");
    try {
      await login(token, remember);
      setToken("");
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setLoading(false);
    }
  };
  return (
    <Dialog open={remote && authRequired}>
      <DialogContent
        showClose={false}
        className="max-w-md"
        onEscapeKeyDown={(event) => event.preventDefault()}
        onPointerDownOutside={(event) => event.preventDefault()}
      >
        <DialogHeader>
          <div className="mb-2 grid size-10 place-items-center rounded-xl bg-primary text-primary-foreground">
            <RadioTower className="size-5" />
          </div>
          <DialogTitle>
            {t("auth.connectTitle")}
          </DialogTitle>
          <DialogDescription>
            {t("auth.connectDescription")}
          </DialogDescription>
        </DialogHeader>
        <form
          className="grid gap-4"
          onKeyDown={(event) => {
            if (event.key === "Enter" && event.nativeEvent.isComposing)
              event.preventDefault();
          }}
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
          <>
              <Field>
                <Label htmlFor="remote-token">{t("auth.token")}</Label>
                <Input
                  id="remote-token"
                  type="password"
                  autoComplete="off"
                  autoFocus
                  value={token}
                  onChange={(event) => setToken(event.target.value)}
                />
              </Field>
              <label className="flex min-h-10 cursor-pointer items-start gap-3 rounded-lg text-sm" data-remote-token-remember="">
                <input
                  type="checkbox"
                  className="mt-0.5 size-4 shrink-0 accent-primary"
                  checked={remember}
                  onChange={(event) => setRemember(event.target.checked)}
                />
                <span className="min-w-0">
                  <span className="block text-foreground">{t("auth.remember")}</span>
                  <span className="mt-0.5 block text-xs leading-relaxed text-muted-foreground">{t("auth.rememberDescription")}</span>
                </span>
              </label>
          </>
          {(error || connectionError) && (
            <p role="alert" className="text-sm text-destructive">
              {error || connectionError}
            </p>
          )}
          <Button disabled={loading || !token.trim()}>
            {loading && <LoaderCircle className="animate-spin" />}
            {t("auth.connect")}
          </Button>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function WorkbenchShell({ runtime }: { runtime: SessionRuntime }) {
  const {
    loading,
    error,
    authRequired,
    accountScope,
    platform,
    currentTenantRole,
    currentSessionId,
    currentSession,
    currentWorkspace,
    forkOperation,
    completeForkHydration,
    createSession,
    createWorkspace,
    refresh,
    notify,
  } = useWorkbench();
  const canOperate = !platform || (currentTenantRole !== null && currentTenantRole !== "viewer");
  const t = useTranslate("app");
  const conversationT = useTranslate("conversation");
  const { locale } = useLocale();
  const [mobileSidebar, setMobileSidebar] = React.useState(false);
  const [settings, setSettings] = React.useState(false);
  const [settingsSection, setSettingsSection] = React.useState<SettingsSection>("general");
  const [picker, setPicker] = React.useState(false);
  const [pickerCreatesSession, setPickerCreatesSession] = React.useState(false);
  const [retrying, setRetrying] = React.useState(false);
  const [online, setOnline] = React.useState(() => navigator.onLine);
  const [updateReady, setUpdateReady] = React.useState(false);
  const selectionScope = JSON.stringify([accountScope ?? "host", currentSessionId]);
  const [detailsState, setDetailsState] = React.useState<{ scope: string; selection: DetailsSelection }>({ scope: selectionScope, selection: null });
  const selection = detailsState.scope === selectionScope ? detailsState.selection : null;
  const setSelection = React.useCallback((selection: DetailsSelection) => {
    setDetailsState({ scope: selectionScope, selection });
  }, [selectionScope]);
  const workspacePanel = useWorkspacePanel(
    !loading && !authRequired && (!platform || currentTenantRole !== null) ? currentSession?.identity.session_id ?? null : null,
    accountScope ?? undefined,
    JSON.stringify([currentWorkspace?.access, currentWorkspace?.status, runtime.liveStatus]),
    () => { setSelection(null); setMobileSidebar(false); },
    message => notify(message, "error"),
  );
  const detailsOpen = Boolean(selection) || workspacePanel.state.open;
  const layout = useWorkbenchLayout(detailsOpen, workspacePanel.state.open);

  React.useEffect(applyStoredTheme, []);
  React.useEffect(() => {
    document.title = t("document.title");
    document
      .querySelector<HTMLMetaElement>('meta[name="description"]')
      ?.setAttribute("content", t("document.description"));
  }, [locale, t]);
  React.useEffect(() => {
    const media = matchMedia("(prefers-color-scheme: dark)");
    const listener = () => {
      if ((localStorage.getItem(storage.theme) ?? "system") === "system")
        applyStoredTheme();
    };
    media.addEventListener("change", listener);
    return () => media.removeEventListener("change", listener);
  }, []);
  React.useEffect(() => {
    const childSessionId = forkOperation?.phase === "hydrating"
      ? forkOperation.childSessionId
      : null;
    if (!childSessionId || runtime.loading || runtime.loadedSessionId !== childSessionId)
      return;
    completeForkHydration(childSessionId);
  }, [completeForkHydration, forkOperation, runtime.loadedSessionId, runtime.loading]);
  React.useEffect(() => {
    const sync = () => setOnline(navigator.onLine);
    window.addEventListener("online", sync);
    window.addEventListener("offline", sync);
    return () => {
      window.removeEventListener("online", sync);
      window.removeEventListener("offline", sync);
    };
  }, []);
  React.useEffect(() => {
    const listener = (event: KeyboardEvent) => {
      if (!canOperate) return;
      if (
        (event.ctrlKey || event.metaKey) &&
        event.key.toLocaleLowerCase() === "k"
      ) {
        event.preventDefault();
        if (currentWorkspace)
          void createSession().catch((cause) =>
            notify(
              cause instanceof Error ? cause.message : String(cause),
              "error",
            ),
          );
        else {
          setPickerCreatesSession(true);
          setPicker(true);
        }
      }
    };
    window.addEventListener("keydown", listener);
    return () => window.removeEventListener("keydown", listener);
  }, [canOperate, createSession, currentWorkspace, notify]);

  React.useEffect(() => {
    const invoke = window.__TAURI__?.core?.invoke;
    const listen = window.__TAURI__?.event?.listen;
    if (!invoke || !canOperate) return;
    const handle = async (payload: unknown) => {
      for (const raw of Array.isArray(payload) ? payload : []) {
        try {
          const url = new URL(String(raw));
          const action =
            url.hostname || url.pathname.split("/").filter(Boolean)[0];
          const path =
            url.protocol === "ternilo:" && action === "workspace"
              ? url.searchParams.get("path")
              : null;
          if (path) {
            await createWorkspace(path);
            notify(t("workspace.deepLinkOpened", { path }));
          }
        } catch {
          /* Ignore unrelated deep links. */
        }
      }
    };
    let unlisten: (() => void) | undefined;
    void invoke<unknown>("desktop_initial_links")
      .then(handle)
      .catch(() => undefined);
    if (listen)
      void listen<unknown>(
        "deep-link://new-url",
        (event) => void handle(event.payload),
      ).then((value) => {
        unlisten = value;
      });
    return () => unlisten?.();
  }, [canOperate, createWorkspace, notify, t]);

  React.useEffect(() => {
    if (!canOperate) setPicker(false);
  }, [canOperate]);

  React.useEffect(() => {
    if (authRequired) setSettings(false);
  }, [authRequired]);

  React.useEffect(() => {
    let dispose: (() => void) | undefined;
    let cancelled = false;
    void registerPwa(() => setUpdateReady(true)).then((next) => {
      if (cancelled) next();
      else dispose = next;
    });
    return () => {
      cancelled = true;
      dispose?.();
    };
  }, []);

  const openPicker = (createAfter = true) => {
    if (!canOperate) return;
    setMobileSidebar(false);
    setPickerCreatesSession(createAfter);
    setPicker(true);
  };
  const openSettings = (section: SettingsSection = "general") => {
    if (platform) {
      setMobileSidebar(false);
      navigate(section === 'models' ? '/models' : `/settings/${section}`);
      return;
    }
    setSettingsSection(section);
    setSettings(true);
  };

  return (
    <>
      <a className="skip-link" href="#conversation-main">
        {t("navigation.skipConversation")}
      </a>
      <WorkspacePanelContext.Provider value={workspacePanel}><AppFrame
        layout={layout}
        mobileSidebarOpen={mobileSidebar}
        detailsOpen={detailsOpen}
        detailsFullscreen={workspacePanel.state.open && workspacePanel.state.fullscreen}
        labels={{
          closeMobileSidebar: t("sidebar.close"),
          resizeSidebar: t("layout.resizeSidebar"),
          resizeDetails: t("layout.resizeDetails"),
        }}
        onCloseMobileSidebar={() => setMobileSidebar(false)}
        sidebar={<Sidebar
          key={accountScope ?? "host"}
          collapsed={layout.sidebarCollapsed}
          mobileOpen={mobileSidebar}
          currentSessionEvents={runtime.events}
          liveStatus={runtime.liveStatus}
          onCollapsedChange={layout.setSidebarCollapsed}
          onMobileOpenChange={setMobileSidebar}
          onChooseWorkspace={openPicker}
          onOpenSettings={() => openSettings()}
        />}
        conversation={<ConversationColumn
          key={accountScope ?? "host"}
          runtime={runtime}
          selection={selection}
          onSelect={next => {
            if (next) workspacePanel.update(state => ({ ...state, open: false }));
            setSelection(next);
          }}
          onOpenMobileSidebar={() => setMobileSidebar(true)}
          onChooseWorkspace={() => openPicker(true)}
          onOpenModels={() => openSettings("models")}
        />}
        details={<>
          {workspacePanel.state.open && <WorkspacePanel key={workspacePanel.key} overlay={layout.detailsOverlay || workspacePanel.state.fullscreen} />}
          <DetailsPanel
            selection={workspacePanel.state.open ? null : selection}
            sessionId={currentSessionId ?? undefined}
            onClose={() => setSelection(null)}
          />
        </>}
      /></WorkspacePanelContext.Provider>
      {forkOperation?.phase === "creating" && forkOperation.sourceSessionId !== currentSessionId && (
        <div
          data-fork-progress="creating"
          className="fixed left-1/2 top-[max(1rem,env(safe-area-inset-top))] z-[85] flex w-[min(520px,calc(100vw-2rem))] -translate-x-1/2 items-start gap-3 rounded-xl border bg-popover px-4 py-3 text-sm shadow-xl"
          role="status"
          aria-live="polite"
          aria-busy="true"
        >
          <LoaderCircle className="mt-0.5 size-4 shrink-0 animate-spin text-primary" />
          <span className="min-w-0">
            <strong className="block text-foreground">{conversationT("session.forking")}</strong>
            <span className="mt-0.5 block leading-relaxed text-muted-foreground">{conversationT("session.forkingDescription")}</span>
          </span>
        </div>
      )}
      {loading && !authRequired && (
        <div className="fixed inset-0 z-[80] grid place-items-center bg-background/70 p-4 backdrop-blur-sm" role="status" aria-live="polite" aria-busy="true">
          <div className="flex max-w-full items-center gap-2 rounded-xl border bg-card px-4 py-3 text-sm shadow-xl">
            <LoaderCircle className="size-4 animate-spin text-primary" />
            {t("connecting")}
          </div>
        </div>
      )}
      {updateReady && (
        <div data-pwa-update="" className="fixed bottom-[max(1rem,env(safe-area-inset-bottom))] left-1/2 z-[85] flex w-[min(560px,calc(100vw-2rem))] -translate-x-1/2 flex-wrap items-center gap-3 rounded-xl border bg-popover px-4 py-3 text-sm shadow-xl" role="status" aria-live="polite">
          <span className="min-w-0 flex-1">{t("update.ready")}</span>
          <Button type="button" variant="outline" className="min-h-10 shrink-0" onClick={() => location.reload()}>
            {t("update.reload")}
          </Button>
        </div>
      )}
      {error && !loading && !authRequired && (
        <div data-offline-shell={window.__TERNILO_BOOT__?.offline || !online || undefined} className="fixed left-1/2 top-[max(1rem,env(safe-area-inset-top))] z-[80] flex w-[min(560px,calc(100vw-2rem))] flex-wrap -translate-x-1/2 items-center gap-3 rounded-xl border border-destructive/35 bg-popover px-4 py-3 text-sm text-destructive shadow-xl" role="alert">
          <AlertCircle className="size-4 shrink-0" />
          <span className="min-w-[12rem] flex-1 break-words">{window.__TERNILO_BOOT__?.offline || !online ? t("error.offline") : error}</span>
          <Button
            type="button"
            variant="outline"
            className="min-h-10 shrink-0"
            disabled={retrying}
            onClick={() => {
              if (window.__TERNILO_BOOT__?.offline) {
                location.reload();
                return;
              }
              setRetrying(true);
              void refresh().catch(() => undefined).finally(() => setRetrying(false));
            }}
          >
            {retrying && <LoaderCircle className="animate-spin" />}
            {t("error.retry")}
          </Button>
        </div>
      )}
      <DirectoryPicker
        key={`directory:${accountScope ?? "host"}`}
        open={picker && canOperate}
        onOpenChange={setPicker}
        createSessionAfter={pickerCreatesSession}
      />
      <SettingsDialog
        effectiveProfile={runtime.effectiveProfile}
        key={`settings:${accountScope ?? "host"}`}
        open={settings}
        onOpenChange={setSettings}
        onSessionChanged={async () => {
          await Promise.all([runtime.reloadMetadata(), refresh()]);
        }}
        initialSection={settingsSection}
      />
    </>
  );
}

function PersistentWorkbench({ hidden }: { hidden: boolean }) {
  const runtime = useSessionRuntime();
  return <React.Activity mode={hidden ? 'hidden' : 'visible'}><WorkbenchShell runtime={runtime} /></React.Activity>;
}

function ApplicationRouter() {
  const path = usePathname();
  const { platform } = useWorkbench();
  React.useEffect(applyStoredTheme, []);
  const legacyModels = platform && path === '/settings/models';
  React.useEffect(() => { if (legacyModels) navigate('/models'); }, [legacyModels]);
  const userSettings = platform && !legacyModels && (path === '/settings' || path.startsWith('/settings/'));
  const files = path === '/files';
  const deviceApproval = platform && path === '/model-connect';
  const emailMode = platform ? path === '/auth/verify-email' ? 'verify' : path === '/auth/reset-password' ? 'reset' : path === '/auth/recover' ? 'recover' : null : null;
  const standalone = Boolean(emailMode) || deviceApproval || files || userSettings || legacyModels || path === '/admin' || path.startsWith('/admin/') || path === '/spaces/current' || path === '/models';
  const [workbenchVisited, setWorkbenchVisited] = React.useState(!standalone);
  React.useEffect(() => { if (!standalone) setWorkbenchVisited(true); }, [standalone]);
  return <>
    {(workbenchVisited || !standalone) && <PersistentWorkbench hidden={standalone} />}
    {standalone && (emailMode ? <AccountEmailPage key={emailMode} mode={emailMode} /> : deviceApproval ? <ModelDeviceApproval /> : files ? <FilesPage /> : userSettings ? <UserSettingsPage section={(path.split('/')[2] || 'general') as SettingsSection} /> : path === '/models' || legacyModels ? <ModelAccessShell /> : <AdminShell path={path} />)}
    {platform ? (!emailMode || emailMode === 'verify') && <ServerLogin /> : <RemoteLogin />}
    <ToastRegion />
  </>;
}

export function App() {
  return (
    <LocaleProvider>
      <TooltipProvider delayDuration={250}>
        <WorkbenchProvider>
          <ApplicationRouter />
        </WorkbenchProvider>
      </TooltipProvider>
    </LocaleProvider>
  );
}
