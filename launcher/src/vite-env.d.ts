/// <reference types="vite/client" />

interface ImportMetaEnv {
  readonly VITE_VESPER_DEMO_TOUR?: boolean | string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
