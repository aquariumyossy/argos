/// <reference types="vite/client" />

import "react";

declare module "react" {
  interface IframeHTMLAttributes<T> {
    /** Embedded document policy. Not in the DOM lib yet. */
    csp?: string;
  }
}
