/**
 * A key/value table for HTTP headers (or query params).
 */

export function HeadersTable({
  headers,
  caption,
  emptyLabel = "No headers captured.",
}: {
  headers: Record<string, string>;
  caption?: string;
  emptyLabel?: string;
}) {
  const keys = Object.keys(headers).sort();
  return (
    <div data-testid="headers-table">
      {caption && (
        <div className="mb-1 text-[10px] uppercase tracking-wide text-slate-500">{caption}</div>
      )}
      {keys.length === 0 ? (
        <p className="text-xs text-slate-500">{emptyLabel}</p>
      ) : (
        <table className="w-full table-fixed border-collapse text-xs">
          <tbody>
            {keys.map((key) => (
              <tr key={key} className="border-b border-slate-800/70 align-top last:border-0">
                <th
                  scope="row"
                  className="w-1/3 py-1 pr-3 text-left font-normal text-violet-300"
                >
                  {key}
                </th>
                <td className="break-all py-1 font-mono text-slate-200">{headers[key]}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}
