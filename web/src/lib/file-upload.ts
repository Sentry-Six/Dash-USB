/** Preserve a picked folder's path; never flatten nested files into the destination. */
export function uploadRelativePath(file: Pick<File, "name" | "webkitRelativePath">): string {
  const path = file.webkitRelativePath || file.name
  const hasControl = [...path].some(character => character.charCodeAt(0) < 32 || character.charCodeAt(0) === 127)
  if (!path || path.includes("\\") || hasControl ||
      path.split("/").some(part => !part || part === "." || part === "..")) {
    throw new Error("Invalid upload path. Choose files or a folder again.")
  }
  return path
}
