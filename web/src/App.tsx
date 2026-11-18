import { useEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";
import {
  desktopPlatforms,
  formatBytes,
  localized,
  matches,
  parseRoute,
  platformLabel,
  playable,
  routePath,
  type AppDetail,
  type AppSummary,
  type Media,
  type Route,
} from "./logic";
import { Markdown } from "./Markdown";
import { appDetail, listApps, playEvent, playUrl, siteConfig, verifiedMedia, type Progress } from "./vesper";

const languages = typeof navigator === "undefined" ? ["en"] : navigator.languages ?? [navigator.language];
const APP_DOWNLOAD = "https://github.com/Baryon-Interactive/vesper/releases/latest";

function useRoute(): [Route, (route: Route) => void] {
  const [route, setRoute] = useState(() => parseRoute(location.pathname));
  useEffect(() => {
    const onPop = () => setRoute(parseRoute(location.pathname));
    addEventListener("popstate", onPop);
    return () => removeEventListener("popstate", onPop);
  }, []);
  const go = (next: Route) => {
    history.pushState(null, "", routePath(next));
    setRoute(next);
    scrollTo(0, 0);
  };
  return [route, go];
}

function Link({ to, go, className, children }: { to: Route; go: (r: Route) => void; className?: string; children: ReactNode }) {
  const onClick = (event: MouseEvent) => {
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.button !== 0) return;
    event.preventDefault();
    go(to);
  };
  return (
    <a href={routePath(to)} className={className} onClick={onClick}>
      {children}
    </a>
  );
}

function Cover({ media, title, className }: { media?: Media; title: string; className: string }) {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    setSrc(null);
    if (media?.url && media.blake3) verifiedMedia(media.url, media.blake3).then((url) => live && setSrc(url), () => {});
    return () => {
      live = false;
    };
  }, [media?.url, media?.blake3]);
  return src ? (
    <img className={className} src={src} alt="" />
  ) : (
    <div className={`${className} cover-fallback`} aria-hidden>
      {title.slice(0, 1)}
    </div>
  );
}

function Badges({ app }: { app: AppSummary }) {
  return (
    <div className="badges">
      {playable(app) && <span className="badge web">Plays in browser</span>}
      {desktopPlatforms(app).map((p) => (
        <span className="badge" key={p}>
          {platformLabel(p)}
        </span>
      ))}
    </div>
  );
}

function Header({ go, route }: { go: (r: Route) => void; route: Route }) {
  return (
    <header className="header">
      <Link to={{ page: "store" }} go={go} className="brand">
        <img className="brand-mark" src="/brand/vesper-mark.svg" alt="" />
        <span>
          Vesper <small>STORE</small>
        </span>
      </Link>
      <nav>
        <Link to={{ page: "store" }} go={go} className={route.page === "store" ? "nav active" : "nav"}>
          Browse
        </Link>
        <a className="nav" href="https://github.com/Baryon-Interactive/vesper/blob/main/docs/publishing.md" target="_blank" rel="noopener noreferrer">
          Publish a game
        </a>
      </nav>
      <a className="secondary get-app" href={APP_DOWNLOAD} target="_blank" rel="noopener noreferrer">
        Get the Vesper app
      </a>
    </header>
  );
}

function StorePage({ go }: { go: (r: Route) => void }) {
  const [apps, setApps] = useState<AppSummary[] | null>(null);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [webOnly, setWebOnly] = useState(false);
  useEffect(() => {
    listApps().then(setApps, (e: Error) => setError(e.message));
  }, []);
  const verified = (apps ?? []).filter((a) => !a.error);
  const rejected = (apps ?? []).filter((a) => a.error);
  const shown = verified.filter((a) => matches(a, query, languages) && (!webOnly || playable(a)));
  const featured = verified.find(playable) ?? verified[0];
  return (
    <main className="page">
      {featured && !query && !webOnly && <Hero app={featured} go={go} />}
      <div className="section-head">
        <div>
          <p className="eyebrow">CATALOGUE</p>
          <h2>
            All games <small>{apps ? `${shown.length} of ${verified.length}` : "verifying"}</small>
          </h2>
        </div>
        <div className="toolbar">
          <label className="search">
            <span aria-hidden>⌕</span>
            <input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search games, developers, tags" />
          </label>
          <button className={webOnly ? "filter on" : "filter"} onClick={() => setWebOnly(!webOnly)} aria-pressed={webOnly}>
            Plays in browser
          </button>
        </div>
      </div>
      {error && <p className="notice error">Could not verify the catalogue: {error}</p>}
      {!apps && !error && <p className="notice">Verifying catalogue signatures in your browser…</p>}
      {apps && !verified.length && !error && <p className="notice">The catalogue has no games yet.</p>}
      <div className="game-grid">
        {shown.map((app) => {
          const title = localized(app.store?.title, languages, app.app_id);
          return (
            <Link key={app.app_id} to={{ page: "app", appId: app.app_id }} go={go} className="game-card">
              <Cover media={app.store?.cover} title={title} className="card-cover" />
              <div className="card-body">
                <h3>{title}</h3>
                <p>{localized(app.store?.summary, languages)}</p>
                <div className="card-foot">
                  <Badges app={app} />
                  <span className="price">{app.store?.price_display ?? ""}</span>
                </div>
              </div>
            </Link>
          );
        })}
      </div>
      {rejected.length > 0 && (
        <details className="rejected">
          <summary>{rejected.length} listing(s) failed verification and are hidden</summary>
          <ul>
            {rejected.map((a) => (
              <li key={a.app_id}>
                <code>{a.app_id}</code>: {a.error}
              </li>
            ))}
          </ul>
        </details>
      )}
      <footer className="footer">
        Listings, builds and media are checked against the signed Vesper catalogue in your browser before they are shown or run.
      </footer>
    </main>
  );
}

function Hero({ app, go }: { app: AppSummary; go: (r: Route) => void }) {
  const title = localized(app.store?.title, languages, app.app_id);
  return (
    <section className="hero">
      <Cover media={app.store?.cover} title={title} className="hero-cover" />
      <div className="hero-shade" />
      <div className="hero-text">
        <p className="eyebrow accent">{playable(app) ? "PLAY NOW · NO INSTALL" : "FEATURED"}</p>
        <h1>{title}</h1>
        <p>{localized(app.store?.summary, languages)}</p>
        <div className="actions">
          {playable(app) && (
            <Link to={{ page: "play", appId: app.app_id }} go={go} className="primary">
              Play in browser
            </Link>
          )}
          <Link to={{ page: "app", appId: app.app_id }} go={go} className="secondary">
            View details
          </Link>
        </div>
      </div>
    </section>
  );
}

function AppPage({ appId, go }: { appId: string; go: (r: Route) => void }) {
  const [app, setApp] = useState<AppDetail | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    setApp(null);
    setError("");
    appDetail(appId).then(setApp, (e: Error) => setError(e.message));
  }, [appId]);
  if (error)
    return (
      <main className="page">
        <p className="notice error">
          <code>{appId}</code> could not be verified: {error}
        </p>
      </main>
    );
  if (!app)
    return (
      <main className="page">
        <p className="notice">Verifying {appId}…</p>
      </main>
    );
  const store = app.store ?? {};
  const title = localized(store.title, languages, app.app_id);
  document.title = `${title} · Vesper`;
  const desktop = desktopPlatforms(app);
  const web = app.web_channel ? app.channels?.[app.web_channel] : undefined;
  return (
    <main className="page detail">
      <section className="detail-hero">
        <Cover media={store.cover} title={title} className="detail-cover" />
        <div className="detail-head">
          <p className="eyebrow">{store.developer ?? "Unknown developer"}</p>
          <h1>{title}</h1>
          <p className="summary">{localized(store.summary, languages)}</p>
          <div className="actions">
            {app.web_channel && (
              <Link to={{ page: "play", appId }} go={go} className="primary">
                Play in browser
              </Link>
            )}
            {desktop.length > 0 && (
              <a className="secondary" href={APP_DOWNLOAD} target="_blank" rel="noopener noreferrer">
                Install with the Vesper app
              </a>
            )}
            <span className="price">{store.price_display}</span>
          </div>
          <Badges app={app} />
        </div>
      </section>
      <div className="detail-body">
        <article>
          <Markdown source={app.description} skipTitle={title} />
          {(store.screenshots ?? []).length > 0 && (
            <div className="shots">
              {store.screenshots!.map((shot) => (
                <Cover key={shot.url} media={shot} title={title} className="shot" />
              ))}
            </div>
          )}
        </article>
        <aside>
          <dl>
            {web && (
              <>
                <dt>Browser build</dt>
                <dd>
                  Channel <code>{app.web_channel}</code>, build {web.version}
                </dd>
              </>
            )}
            {desktop.length > 0 && (
              <>
                <dt>Desktop</dt>
                <dd>{desktop.map(platformLabel).join(", ")}</dd>
              </>
            )}
            {Object.entries(store.requirements ?? {}).map(([key, value]) => (
              <div key={key} className="req">
                <dt>{key}</dt>
                <dd>{value}</dd>
              </div>
            ))}
            {(store.tags ?? []).length > 0 && (
              <>
                <dt>Tags</dt>
                <dd className="tags">
                  {store.tags!.map((t) => (
                    <span key={t}>{t}</span>
                  ))}
                </dd>
              </>
            )}
            {store.homepage && (
              <>
                <dt>Website</dt>
                <dd>
                  <a href={store.homepage} target="_blank" rel="noopener noreferrer">
                    {store.homepage.replace(/^https?:\/\//, "")}
                  </a>
                </dd>
              </>
            )}
            <dt>Verification</dt>
            <dd className="verified">
              <i /> Signed listing, targets version {app.targets_version}
            </dd>
          </dl>
        </aside>
      </div>
    </main>
  );
}

function PlayPage({ appId, go }: { appId: string; go: (r: Route) => void }) {
  const [src, setSrc] = useState("");
  const [ready, setReady] = useState<{ version: string; channel: string } | null>(null);
  const [title, setTitle] = useState(appId);
  const [error, setError] = useState("");
  const [progress, setProgress] = useState<Progress>({ downloaded: 0, cached: 0, requests: 0 });
  const [started, setStarted] = useState(false);
  const [isolated, setIsolated] = useState<boolean | null>(null);
  const frame = useRef<HTMLIFrameElement>(null);
  useEffect(() => {
    let playOrigin = "";
    const onMessage = (event: MessageEvent) => {
      // Only the play frame we created, from the configured play origin.
      if (!playOrigin || event.origin !== playOrigin || event.source !== frame.current?.contentWindow) return;
      const message = playEvent(event.data);
      if (message?.type === "progress") setProgress(message.progress);
      if (message?.type === "ready") setReady({ version: message.version, channel: message.channel });
      if (message?.type === "started") {
        setStarted(true);
        setIsolated(message.isolated);
        frame.current?.focus();
      }
      if (message?.type === "error") setError(message.message);
    };
    addEventListener("message", onMessage);
    appDetail(appId).then((a) => {
      const name = localized(a.store?.title, languages, appId);
      setTitle(name);
      document.title = `${name} · Vesper`;
    }, () => {});
    siteConfig().then((config) => {
      playOrigin = config.playOrigin;
      setSrc(playUrl(config.playOrigin, appId));
    }, (e: Error) => setError(e.message));
    return () => removeEventListener("message", onMessage);
  }, [appId]);
  const status = error
    ? `Error: ${error}`
    : !ready
      ? "Verifying catalogue and build manifest…"
      : `Build ${ready.version} · ${formatBytes(progress.downloaded)} downloaded · ${formatBytes(progress.cached)} from cache`;
  return (
    <div className="play">
      <div className="play-bar">
        <Link to={{ page: "app", appId }} go={go} className="back">
          ← {title}
        </Link>
        <span
          className={error ? "play-status error" : "play-status"}
          title={
            isolated === null
              ? undefined
              : isolated
                ? "Cross-origin isolated: SharedArrayBuffer and WebAssembly threads are available"
                : "Not cross-origin isolated in this browser: no SharedArrayBuffer or WebAssembly threads"
          }
        >
          <i className={ready && !error ? "live" : ""} />
          {status}
        </span>
        <button className="secondary" onClick={() => frame.current?.requestFullscreen()} disabled={!started}>
          Fullscreen
        </button>
      </div>
      {src && (
        <iframe
          ref={frame}
          className="play-frame"
          src={src}
          title={title}
          allow="autoplay; fullscreen; gamepad; microphone; cross-origin-isolated"
          referrerPolicy="origin"
        />
      )}
      {!started && !error && (
        <div className="play-overlay">
          <div className="spinner" />
          <p>{ready ? "Streaming the first files…" : "Checking signatures in your browser…"}</p>
        </div>
      )}
    </div>
  );
}

export default function App() {
  const [route, go] = useRoute();
  useEffect(() => {
    if (route.page === "store") document.title = "Vesper";
  }, [route]);
  if (route.page === "play") return <PlayPage appId={route.appId} go={go} />;
  return (
    <div className="site">
      <Header go={go} route={route} />
      {route.page === "app" ? <AppPage appId={route.appId} go={go} /> : <StorePage go={go} />}
    </div>
  );
}
