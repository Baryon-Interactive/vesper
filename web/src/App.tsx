import { useEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";
import {
  changelog,
  desktopPlatforms,
  formatBytes,
  formatDate,
  gallery,
  label,
  languageName,
  localized,
  matches,
  parseRoute,
  platformLabel,
  playable,
  releaseDateLabel,
  releaseLine,
  routePath,
  youtubeEmbed,
  youtubeThumbnail,
  type AppDetail,
  type AppSummary,
  type GalleryItem,
  type Media,
  type Route,
  type Spec,
  type Store,
  type Trailer,
} from "./logic";
import { Markdown } from "./Markdown";
import { appDetail, listApps, playEvent, playUrl, siteConfig, verifiedMedia, type Progress } from "./vesper";

const languages = typeof navigator === "undefined" ? ["en"] : navigator.languages ?? [navigator.language];
// Dates and language names follow the UI language, English until the UI is translated.
const uiLocale = "en";
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

/**
 * In-app link. The play page is cross-origin isolated (COOP/COEP) and the
 * other pages are not, so links into and out of it load a new document.
 */
function Link({ to, go, className, children }: { to: Route; go: (r: Route) => void; className?: string; children: ReactNode }) {
  const onClick = (event: MouseEvent) => {
    if ((to.page === "play") !== (parseRoute(location.pathname).page === "play")) return;
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

/** Object URL of a store image after its signed BLAKE3 checks out, or null until then. */
function useVerified(media?: Media): string | null {
  const [src, setSrc] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    setSrc(null);
    if (media?.url && media.blake3) verifiedMedia(media.url, media.blake3).then((url) => live && setSrc(url), () => {});
    return () => {
      live = false;
    };
  }, [media?.url, media?.blake3]);
  return src;
}

function Cover({ media, title, className }: { media?: Media; title: string; className: string }) {
  const src = useVerified(media);
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
  const release = releaseLine(store.release, uiLocale);
  const updates = changelog(app.documents, languages);
  return (
    <main className="page detail">
      <Backdrop media={store.background} />
      <section className="detail-hero">
        <Gallery items={gallery(store)} cover={store.cover} title={title} />
        <div className="detail-head">
          <p className="eyebrow">{store.developer ?? "Unknown developer"}</p>
          <Logo media={store.logo} title={title} />
          {release && <p className={`release ${store.release?.status ?? ""}`}>{release}</p>}
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
          <SystemRequirements store={store} />
          {updates && (
            <section className="updates">
              <h2>Updates</h2>
              <Collapsible>
                <Markdown source={updates} />
              </Collapsible>
            </section>
          )}
        </article>
        <aside>
          <Facts app={app} store={store} web={web ? `Channel ${app.web_channel}, build ${web.version}` : ""} desktop={desktop} />
        </aside>
      </div>
    </main>
  );
}

function Backdrop({ media }: { media?: Media }) {
  const src = useVerified(media);
  return src ? <div className="backdrop" style={{ backgroundImage: `url(${src})` }} aria-hidden /> : null;
}

/** The title logo when the listing has one; the heading text stays for screen readers. */
function Logo({ media, title }: { media?: Media; title: string }) {
  const src = useVerified(media);
  return (
    <h1 className={src ? "has-logo" : undefined}>
      {src && <img className="logo" src={src} alt="" />}
      <span>{title}</span>
    </h1>
  );
}

/** Trailers and screenshots: one large view and a strip of thumbnails. */
function Gallery({ items, cover, title }: { items: GalleryItem[]; cover?: Media; title: string }) {
  const [selected, setSelected] = useState(0);
  const [playing, setPlaying] = useState(false);
  const current = items[Math.min(selected, items.length - 1)];
  const pick = (index: number) => {
    setSelected(index);
    setPlaying(false);
  };
  return (
    <div className="gallery">
      {!current ? (
        <Cover media={cover} title={title} className="detail-cover" />
      ) : current.kind === "image" ? (
        <Cover media={current.media} title={title} className="detail-cover" />
      ) : (
        <TrailerView key={selected} trailer={current.trailer} playing={playing} onPlay={() => setPlaying(true)} title={title} />
      )}
      {items.length > 1 && (
        <div className="thumbs">
          {items.map((item, index) => (
            <button
              key={index}
              className={index === selected ? "thumb on" : "thumb"}
              aria-pressed={index === selected}
              onClick={() => pick(index)}
              aria-label={item.kind === "trailer" ? `Trailer ${index + 1}` : `Screenshot ${index + 1}`}
            >
              {item.kind === "image" ? (
                <Cover media={item.media} title={title} className="thumb-img" />
              ) : (
                <TrailerThumb trailer={item.trailer} title={title} />
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function TrailerThumb({ trailer, title }: { trailer: Trailer; title: string }) {
  return (
    <span className="thumb-trailer">
      {trailer.kind === "youtube" ? (
        <img className="thumb-img" src={youtubeThumbnail(trailer.id)} alt="" loading="lazy" />
      ) : (
        <Cover media={trailer.poster} title={title} className="thumb-img" />
      )}
      <i className="play-mark" />
    </span>
  );
}

function TrailerView({ trailer, playing, onPlay, title }: { trailer: Trailer; playing: boolean; onPlay: () => void; title: string }) {
  const poster = useVerified(trailer.kind === "video" ? trailer.poster : undefined);
  const name = localized(trailer.title, languages, `${title} trailer`);
  if (trailer.kind === "video")
    return <video className="detail-cover" src={trailer.url} poster={poster ?? undefined} controls preload="none" title={name} />;
  // Nothing loads from YouTube until the player asks for the video.
  if (!playing)
    return (
      <button className="detail-cover facade" onClick={onPlay} aria-label={`Play ${name}`}>
        <img src={youtubeThumbnail(trailer.id)} alt="" />
        <i className="play-mark big" />
        <small>Plays from YouTube</small>
      </button>
    );
  return (
    <iframe
      className="detail-cover"
      src={youtubeEmbed(trailer.id)}
      title={name}
      allow="autoplay; encrypted-media; picture-in-picture; fullscreen"
      referrerPolicy="strict-origin-when-cross-origin"
    />
  );
}

/** Long changelogs start folded. */
function Collapsible({ children }: { children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const [long, setLong] = useState(false);
  const body = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const el = body.current;
    // Matches .collapsible-body max-height in styles.css.
    if (el) setLong(el.scrollHeight > 340);
  }, [children]);
  return (
    <div className={open || !long ? "collapsible open" : "collapsible"}>
      <div className="collapsible-body" ref={body}>
        {children}
      </div>
      {(long || open) && (
        <button className="link-button" onClick={() => setOpen(!open)}>
          {open ? "Show less" : "Show all updates"}
        </button>
      )}
    </div>
  );
}

const SPEC_KEYS: (keyof Spec)[] = ["os", "cpu", "gpu", "memory", "storage", "network", "notes"];

function SystemRequirements({ store }: { store: Store }) {
  const all = store.system_requirements ?? {};
  const platforms = Object.keys(all);
  const [chosen, setChosen] = useState("");
  if (platforms.length === 0) return null;
  const platform = platforms.includes(chosen) ? chosen : platforms[0];
  const tiers = all[platform];
  return (
    <section className="sysreq">
      <h2>System requirements</h2>
      {platforms.length > 1 && (
        <div className="tabs">
          {platforms.map((p) => (
            <button key={p} className={p === platform ? "tab on" : "tab"} onClick={() => setChosen(p)}>
              {platformLabel(p)}
            </button>
          ))}
        </div>
      )}
      <div className="tiers">
        {(["minimum", "recommended"] as const).map((tier) => {
          const spec = tiers[tier];
          if (!spec) return null;
          return (
            <dl key={tier}>
              <dt className="tier">{tier === "minimum" ? "Minimum" : "Recommended"}</dt>
              {SPEC_KEYS.filter((k) => spec[k]).map((k) => (
                <div key={k} className="spec">
                  <dt>{label(k)}</dt>
                  <dd>{spec[k]}</dd>
                </div>
              ))}
            </dl>
          );
        })}
      </div>
    </section>
  );
}

const tick = (on?: boolean) => (on ? "✓" : "");

function Facts({ app, store, web, desktop }: { app: AppDetail; store: Store; web: string; desktop: string[] }) {
  const locale = uiLocale;
  const languageRows = Object.entries(store.languages ?? {});
  const links = Object.entries(store.links ?? {});
  const features = store.features ?? [];
  const players = store.max_players && store.max_players > 1 ? store.max_players : 0;
  return (
    <dl>
      {store.release?.date && (
        <>
          <dt>{releaseDateLabel(store.release.status)}</dt>
          <dd>{formatDate(store.release.date, locale)}</dd>
        </>
      )}
      {store.developer && (
        <>
          <dt>Developer</dt>
          <dd>{store.developer}</dd>
        </>
      )}
      {(store.publishers ?? []).length > 0 && (
        <>
          <dt>Publisher</dt>
          <dd>{store.publishers!.join(", ")}</dd>
        </>
      )}
      {web && (
        <>
          <dt>Browser build</dt>
          <dd>{web}</dd>
        </>
      )}
      {desktop.length > 0 && (
        <>
          <dt>Desktop</dt>
          <dd>{desktop.map(platformLabel).join(", ")}</dd>
        </>
      )}
      {(features.length > 0 || players > 0) && (
        <>
          <dt>Features</dt>
          <dd className="tags">
            {players > 0 && <span>Up to {players} players</span>}
            {features.map((f) => (
              <span key={f}>{label(f)}</span>
            ))}
          </dd>
        </>
      )}
      {languageRows.length > 0 && (
        <>
          <dt>Languages</dt>
          <dd>
            <table className="languages">
              <thead>
                <tr>
                  <th />
                  <th>Interface</th>
                  <th>Audio</th>
                  <th>Subtitles</th>
                </tr>
              </thead>
              <tbody>
                {languageRows.map(([tag, support]) => (
                  <tr key={tag}>
                    <th>{languageName(tag, locale)}</th>
                    <td>{tick(support.interface)}</td>
                    <td>{tick(support.audio)}</td>
                    <td>{tick(support.subtitles)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </dd>
        </>
      )}
      {(store.accessibility ?? []).length > 0 && (
        <>
          <dt>Accessibility</dt>
          <dd className="tags">
            {store.accessibility!.map((a) => (
              <span key={a}>{label(a)}</span>
            ))}
          </dd>
        </>
      )}
      {(store.content_descriptors ?? []).length > 0 && (
        <>
          <dt>Content</dt>
          <dd>{store.content_descriptors!.map(label).join(", ")}</dd>
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
      {(store.homepage || links.length > 0) && (
        <>
          <dt>Links</dt>
          <dd className="links">
            {store.homepage && (
              <a href={store.homepage} target="_blank" rel="noopener noreferrer">
                {store.homepage.replace(/^https?:\/\//, "").replace(/\/$/, "")}
              </a>
            )}
            {links.map(([key, url]) => (
              <a key={key} href={url} target="_blank" rel="noopener noreferrer">
                {label(key)}
              </a>
            ))}
          </dd>
        </>
      )}
      <dt>Verification</dt>
      <dd className="verified">
        <i /> Signed listing, targets version {app.targets_version}
        {store.updated && <>, updated {formatDate(store.updated, locale)}</>}
      </dd>
    </dl>
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
    // The title only; the launch frame never waits on the catalogue worker.
    appDetail(appId).then(
      (a) => {
        const name = localized(a.store?.title, languages, appId);
        setTitle(name);
        document.title = `${name} · Vesper`;
      },
      () => {},
    );
    siteConfig().then((config) => {
      playOrigin = config.playOrigin;
      setSrc(playUrl(config.playOrigin, appId, "stable", location.search));
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
