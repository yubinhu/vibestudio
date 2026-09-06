import { Navigate } from "react-router-dom";

/** Redirect an old top-level route to its renamed one, preserving the rest of the
 *  path + query — so bookmarks/deep links to /studio, /terminals, /secrets and
 *  /credentials still resolve after renaming. Operates on the raw hash
 *  so a hash-mode query (e.g. /terminals?id=…) survives the segment swap. */
export default function RenamedRoute({ to }: { to: string }) {
  const raw = window.location.hash.slice(1) || "/";
  const target = raw.replace(/^\/[^/?#]+/, `/${to}`); // swap the first segment only
  return <Navigate to={target} replace />;
}
