import { useState, useEffect, useRef, useCallback, useMemo } from "react";
import Editor, { loader, type OnMount } from "@monaco-editor/react";
import * as monaco from "monaco-editor";
import {
  readRemoteTextFile,
  writeRemoteTextFile,
  readLocalTextFile,
  writeLocalTextFile,
  listRemoteDir,
  listLocalDir,
  remoteHome,
  localHome,
  remoteMkdir,
  remoteRemove,
  posixJoin,
  searchRemoteDir,
  searchLocalDir,
  type FileEntry,
  type Listing,
  type FileSearchResult,
  type SearchMatch,
  type SearchOptions,
} from "../lib/api";
import "../editor.css";

// Configure Monaco to use locally bundled monaco-editor for full offline capability
loader.config({ monaco });

export interface OpenFileTarget {
  path: string;
  name: string;
  sessionId?: string | null;
  hostLabel?: string | null;
  isLocal?: boolean;
}

interface EditorTabItem {
  id: string; // unique tab id
  path: string;
  name: string;
  content: string;
  savedContent: string;
  isDirty: boolean;
  language: string;
  sessionId?: string | null;
  hostLabel?: string | null;
  isLocal?: boolean;
  loading: boolean;
  error?: string | null;
}

interface Props {
  open: boolean;
  initialFile?: OpenFileTarget | null;
  sessionId?: string | null;
  hostLabel?: string | null;
  isLocal?: boolean;
  onClose: () => void;
}

export function getLanguageForPath(filePath: string): string {
  const fileName = filePath.split("/").pop() || "";
  if (
    fileName.toLowerCase() === "dockerfile" ||
    fileName.toLowerCase().startsWith("dockerfile.")
  ) {
    return "dockerfile";
  }
  if (fileName.startsWith(".env")) return "plaintext";

  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  switch (ext) {
    case "py":
    case "pyw":
      return "python";
    case "c":
    case "h":
      return "c";
    case "cpp":
    case "cc":
    case "cxx":
    case "hpp":
    case "hxx":
      return "cpp";
    case "java":
    case "jav":
      return "java";
    case "groovy":
    case "gvy":
    case "gy":
    case "gsh":
    case "gradle":
      return "groovy";
    case "json":
    case "jsonc":
    case "json5":
      return "json";
    case "yaml":
    case "yml":
      return "yaml";
    case "md":
    case "markdown":
      return "markdown";
    case "txt":
    case "log":
    case "text":
      return "plaintext";
    case "rs":
      return "rust";
    case "go":
      return "go";
    case "sh":
    case "bash":
    case "zsh":
      return "shell";
    case "js":
    case "jsx":
    case "mjs":
    case "cjs":
      return "javascript";
    case "ts":
    case "tsx":
      return "typescript";
    case "html":
    case "htm":
      return "html";
    case "css":
    case "scss":
    case "less":
      return "css";
    case "xml":
    case "svg":
    case "plist":
      return "xml";
    case "sql":
      return "sql";
    case "ini":
    case "conf":
    case "cfg":
    case "properties":
      return "ini";
    case "toml":
      return "toml";
    case "php":
      return "php";
    case "rb":
      return "ruby";
    case "lua":
      return "lua";
    case "swift":
      return "swift";
    case "kt":
    case "kts":
      return "kotlin";
    case "cs":
      return "csharp";
    default:
      return "plaintext";
  }
}

export function getFileIcon(fileName: string): string {
  const ext = fileName.split(".").pop()?.toLowerCase() || "";
  if (fileName.toLowerCase().includes("dockerfile")) return "🐳";
  if (fileName.startsWith(".env")) return "🔒";

  switch (ext) {
    case "py":
      return "🐍";
    case "c":
    case "cpp":
    case "cc":
    case "h":
    case "hpp":
      return "🔷";
    case "java":
      return "☕";
    case "groovy":
    case "gradle":
      return "📜";
    case "json":
      return "📄";
    case "yaml":
    case "yml":
      return "⚙️";
    case "md":
      return "📝";
    case "rs":
      return "🦀";
    case "go":
      return "🐹";
    case "sh":
    case "bash":
    case "zsh":
      return "🐚";
    case "js":
    case "jsx":
      return "🟨";
    case "ts":
    case "tsx":
      return "🔷";
    case "html":
      return "🌐";
    case "css":
    case "scss":
      return "🎨";
    case "sql":
      return "🗄️";
    case "toml":
    case "ini":
    case "conf":
      return "⚙️";
    default:
      return "📄";
  }
}

function formatFileSize(size: number): string {
  if (size === 0) return "--";
  const units = ["B", "KB", "MB", "GB"];
  let n = size;
  let u = 0;
  while (n >= 1024 && u < units.length - 1) {
    n /= 1024;
    u++;
  }
  return `${n < 10 ? n.toFixed(1) : Math.round(n)} ${units[u]}`;
}

const SUPPORTED_LANGUAGES = [
  { id: "plaintext", label: "Plain Text" },
  { id: "python", label: "Python (.py)" },
  { id: "c", label: "C (.c, .h)" },
  { id: "cpp", label: "C++ (.cpp, .hpp)" },
  { id: "java", label: "Java (.java)" },
  { id: "groovy", label: "Groovy (.groovy, .gradle)" },
  { id: "json", label: "JSON (.json)" },
  { id: "yaml", label: "YAML (.yml, .yaml)" },
  { id: "markdown", label: "Markdown (.md)" },
  { id: "rust", label: "Rust (.rs)" },
  { id: "go", label: "Go (.go)" },
  { id: "shell", label: "Shell Script (.sh, .bash)" },
  { id: "typescript", label: "TypeScript (.ts, .tsx)" },
  { id: "javascript", label: "JavaScript (.js, .jsx)" },
  { id: "html", label: "HTML (.html)" },
  { id: "css", label: "CSS (.css, .scss)" },
  { id: "xml", label: "XML (.xml, .svg)" },
  { id: "sql", label: "SQL (.sql)" },
  { id: "dockerfile", label: "Dockerfile" },
  { id: "ini", label: "INI / Config (.ini, .conf)" },
  { id: "toml", label: "TOML (.toml)" },
  { id: "php", label: "PHP (.php)" },
  { id: "ruby", label: "Ruby (.rb)" },
  { id: "lua", label: "Lua (.lua)" },
];

function renderMatchSnippet(match: SearchMatch) {
  const line = match.line_content;
  const start = Math.max(0, match.match_start);
  const end = Math.min(line.length, match.match_end);

  let displayStart = 0;
  let prefix = "";
  if (start > 35) {
    displayStart = start - 15;
    prefix = "...";
  }
  let displayEnd = line.length;
  let suffix = "";
  if (line.length - end > 45) {
    displayEnd = end + 30;
    suffix = "...";
  }

  const before = line.slice(displayStart, start);
  const matched = line.slice(start, end);
  const after = line.slice(end, displayEnd);

  return (
    <span className="editor-match-snippet">
      {prefix}
      {before}
      <mark className="editor-match-highlight">{matched}</mark>
      {after}
      {suffix}
    </span>
  );
}

export function RemoteEditorModal({
  open,
  initialFile,
  sessionId,
  hostLabel,
  isLocal,
  onClose,
}: Props) {
  const [tabs, setTabs] = useState<EditorTabItem[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);
  const [saveStatus, setSaveStatus] = useState<string>("");
  const [cursorPos, setCursorPos] = useState({ line: 1, col: 1 });
  const [wordWrap, setWordWrap] = useState<boolean>(true);
  const [minimap, setMinimap] = useState<boolean>(false);
  const [fontSize, setFontSize] = useState<number>(13);
  const [fullscreen, setFullscreen] = useState<boolean>(false);
  const [pathInput, setPathInput] = useState<string>("");

  // Sidebar View Tab: "explorer" | "search"
  const [sidebarTab, setSidebarTab] = useState<"explorer" | "search">("explorer");

  // Project Explorer Sidebar State
  const [sidebarOpen, setSidebarOpen] = useState<boolean>(true);
  const [explorerPath, setExplorerPath] = useState<string>("");
  const [explorerPathInput, setExplorerPathInput] = useState<string>("");
  const [listing, setListing] = useState<Listing | null>(null);
  const [loadingDir, setLoadingDir] = useState<boolean>(false);
  const [dirError, setDirError] = useState<string | null>(null);
  const [searchFilter, setSearchFilter] = useState<string>("");
  const [showHidden, setShowHidden] = useState<boolean>(true);
  const [newItem, setNewItem] = useState<{ type: "file" | "dir" } | null>(null);
  const [newItemName, setNewItemName] = useState<string>("");

  // Global Search State
  const [searchQuery, setSearchQuery] = useState<string>("");
  const [searchCaseSensitive, setSearchCaseSensitive] = useState<boolean>(false);
  const [searchRegex, setSearchRegex] = useState<boolean>(false);
  const [searchWholeWord, setSearchWholeWord] = useState<boolean>(false);
  const [searchIncludePattern, setSearchIncludePattern] = useState<string>("");
  const [searchTargetDir, setSearchTargetDir] = useState<string>("");
  const [searchResults, setSearchResults] = useState<FileSearchResult[]>([]);
  const [searchLoading, setSearchLoading] = useState<boolean>(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [searchedOnce, setSearchedOnce] = useState<boolean>(false);
  const [collapsedFiles, setCollapsedFiles] = useState<Record<string, boolean>>({});

  const editorRef = useRef<monaco.editor.IStandaloneCodeEditor | null>(null);

  // Define custom terminator dark theme in Monaco
  const handleEditorWillMount = (monacoInstance: typeof monaco) => {
    monacoInstance.editor.defineTheme("terminator-dark", {
      base: "vs-dark",
      inherit: true,
      rules: [
        { token: "comment", foreground: "6b7280", fontStyle: "italic" },
        { token: "keyword", foreground: "bef264", fontStyle: "bold" },
        { token: "string", foreground: "86efac" },
        { token: "number", foreground: "fde047" },
        { token: "type", foreground: "38bdf8" },
        { token: "function", foreground: "93c5fd" },
        { token: "variable", foreground: "f3f4f6" },
        { token: "operator", foreground: "d9f99d" },
      ],
      colors: {
        "editor.background": "#0b0f19",
        "editor.foreground": "#f3f4f6",
        "editorCursor.foreground": "#bef264",
        "editor.lineHighlightBackground": "#11182780",
        "editorLineNumber.foreground": "#4b5563",
        "editorLineNumber.activeForeground": "#bef264",
        "editor.selectionBackground": "#1f293799",
        "editor.inactiveSelectionBackground": "#1f293755",
        "editorGutter.background": "#0b0f19",
        "editorWhitespace.foreground": "#1f2937",
        "editorIndentGuide.background": "#1f2937",
        "editorIndentGuide.activeBackground": "#374151",
      },
    });
  };

  const loadFileContent = useCallback(
    async (
      targetPath: string,
      targetSessionId?: string | null,
      targetIsLocal?: boolean,
    ) => {
      if (targetIsLocal) {
        return await readLocalTextFile(targetPath);
      } else if (targetSessionId) {
        return await readRemoteTextFile(targetSessionId, targetPath);
      } else {
        throw new Error("No active remote session or local file context");
      }
    },
    [],
  );

  const openFile = useCallback(
    async (target: OpenFileTarget) => {
      const existing = tabs.find((t) => t.path === target.path);
      if (existing) {
        setActiveTabId(existing.id);
        setPathInput(existing.path);
        return;
      }

      const tabId = `tab-${Date.now()}-${Math.random().toString(36).substr(2, 5)}`;
      const lang = getLanguageForPath(target.path);

      const newTab: EditorTabItem = {
        id: tabId,
        path: target.path,
        name: target.name || target.path.split("/").pop() || "untitled",
        content: "",
        savedContent: "",
        isDirty: false,
        language: lang,
        sessionId: target.sessionId ?? sessionId,
        hostLabel: target.hostLabel ?? hostLabel,
        isLocal: target.isLocal ?? isLocal,
        loading: true,
        error: null,
      };

      setTabs((prev) => [...prev, newTab]);
      setActiveTabId(tabId);
      setPathInput(target.path);

      try {
        const text = await loadFileContent(
          target.path,
          target.sessionId ?? sessionId,
          target.isLocal ?? isLocal,
        );
        setTabs((prev) =>
          prev.map((t) =>
            t.id === tabId
              ? {
                  ...t,
                  content: text,
                  savedContent: text,
                  isDirty: false,
                  loading: false,
                  error: null,
                }
              : t,
          ),
        );
      } catch (err) {
        setTabs((prev) =>
          prev.map((t) =>
            t.id === tabId
              ? {
                  ...t,
                  loading: false,
                  error: String(err),
                }
              : t,
          ),
        );
      }
    },
    [tabs, sessionId, hostLabel, isLocal, loadFileContent],
  );

  const loadDirectory = useCallback(
    async (dirPath: string) => {
      setLoadingDir(true);
      setDirError(null);
      try {
        let res: Listing;
        if (isLocal || !sessionId) {
          res = await listLocalDir(dirPath);
        } else {
          res = await listRemoteDir(sessionId, dirPath);
        }
        setListing(res);
        setExplorerPath(res.path);
        setExplorerPathInput(res.path);
      } catch (err) {
        setDirError(String(err));
      } finally {
        setLoadingDir(false);
      }
    },
    [sessionId, isLocal],
  );

  // Initialize directory explorer when modal opens
  useEffect(() => {
    if (open) {
      void (async () => {
        try {
          let startDir = explorerPath;
          if (!startDir) {
            if (initialFile?.path) {
              const parts = initialFile.path.split("/").filter(Boolean);
              parts.pop();
              startDir = "/" + parts.join("/");
              if (startDir === "/") startDir = "/";
            } else if (isLocal || !sessionId) {
              startDir = await localHome();
            } else {
              startDir = await remoteHome(sessionId);
            }
          }
          await loadDirectory(startDir || "/");
        } catch {
          await loadDirectory("/");
        }
      })();
    }
  }, [open, sessionId, isLocal, initialFile, loadDirectory]);

  useEffect(() => {
    if (open && initialFile) {
      void openFile(initialFile);
    }
  }, [open, initialFile, openFile]);

  const activeTab = tabs.find((t) => t.id === activeTabId) || null;

  const handleContentChange = (val: string | undefined) => {
    if (val === undefined || !activeTabId) return;
    setTabs((prev) =>
      prev.map((t) =>
        t.id === activeTabId
          ? {
              ...t,
              content: val,
              isDirty: val !== t.savedContent,
            }
          : t,
      ),
    );
  };

  const handleSave = async () => {
    if (!activeTab || activeTab.loading) return;
    setSaveStatus("Saving...");
    try {
      if (activeTab.isLocal) {
        await writeLocalTextFile(activeTab.path, activeTab.content);
      } else if (activeTab.sessionId) {
        await writeRemoteTextFile(
          activeTab.sessionId,
          activeTab.path,
          activeTab.content,
        );
      } else {
        throw new Error("Cannot save: no remote session or local target");
      }

      setTabs((prev) =>
        prev.map((t) =>
          t.id === activeTab.id
            ? { ...t, savedContent: t.content, isDirty: false }
            : t,
        ),
      );
      setSaveStatus("Saved ✓");
      setTimeout(() => setSaveStatus(""), 2000);
    } catch (err) {
      setSaveStatus(`Save failed: ${String(err)}`);
    }
  };

  const handleReload = async () => {
    if (!activeTab) return;
    setTabs((prev) =>
      prev.map((t) => (t.id === activeTab.id ? { ...t, loading: true } : t)),
    );
    try {
      const text = await loadFileContent(
        activeTab.path,
        activeTab.sessionId,
        activeTab.isLocal,
      );
      setTabs((prev) =>
        prev.map((t) =>
          t.id === activeTab.id
            ? {
                ...t,
                content: text,
                savedContent: text,
                isDirty: false,
                loading: false,
                error: null,
              }
            : t,
        ),
      );
      setSaveStatus("Reloaded from server");
      setTimeout(() => setSaveStatus(""), 1500);
    } catch (err) {
      setTabs((prev) =>
        prev.map((t) =>
          t.id === activeTab.id
            ? { ...t, loading: false, error: String(err) }
            : t,
        ),
      );
    }
  };

  const handleCloseTab = (tabId: string, ev?: React.MouseEvent) => {
    ev?.stopPropagation();
    const tabToClose = tabs.find((t) => t.id === tabId);
    if (tabToClose?.isDirty) {
      if (
        !window.confirm(
          `"${tabToClose.name}" has unsaved changes. Are you sure you want to close it?`,
        )
      ) {
        return;
      }
    }

    const nextTabs = tabs.filter((t) => t.id !== tabId);
    setTabs(nextTabs);
    if (activeTabId === tabId) {
      const nextActive = nextTabs.length > 0 ? nextTabs[nextTabs.length - 1].id : null;
      setActiveTabId(nextActive);
      const activeObj = nextTabs.find((t) => t.id === nextActive);
      if (activeObj) setPathInput(activeObj.path);
    }
  };

  const handleNewTab = () => {
    const defaultName = "untitled.txt";
    const defaultPath = explorerPath ? posixJoin(explorerPath, defaultName) : defaultName;
    const tabId = `tab-${Date.now()}`;
    const newTab: EditorTabItem = {
      id: tabId,
      path: defaultPath,
      name: defaultName,
      content: "",
      savedContent: "",
      isDirty: false,
      language: "plaintext",
      sessionId,
      hostLabel,
      isLocal,
      loading: false,
      error: null,
    };
    setTabs((prev) => [...prev, newTab]);
    setActiveTabId(tabId);
    setPathInput(defaultPath);
  };

  const handleOpenPathFromInput = () => {
    if (!pathInput.trim()) return;
    const cleanPath = pathInput.trim();
    const name = cleanPath.split("/").pop() || cleanPath;
    void openFile({
      path: cleanPath,
      name,
      sessionId,
      hostLabel,
      isLocal,
    });
  };

  const handleCreateNewItem = async () => {
    if (!newItem || !newItemName.trim() || !explorerPath) return;
    const targetPath = posixJoin(explorerPath, newItemName.trim());
    try {
      if (newItem.type === "file") {
        if (isLocal || !sessionId) {
          await writeLocalTextFile(targetPath, "");
        } else {
          await writeRemoteTextFile(sessionId, targetPath, "");
        }
        await openFile({
          path: targetPath,
          name: newItemName.trim(),
          sessionId,
          hostLabel,
          isLocal,
        });
      } else {
        if (!isLocal && sessionId) {
          await remoteMkdir(sessionId, targetPath);
        }
      }
      setNewItem(null);
      setNewItemName("");
      await loadDirectory(explorerPath);
    } catch (err) {
      alert(`Failed to create ${newItem.type}: ${String(err)}`);
    }
  };

  const handleDeleteEntry = async (entry: FileEntry, ev: React.MouseEvent) => {
    ev.stopPropagation();
    if (
      !window.confirm(
        `Are you sure you want to delete "${entry.name}" (${entry.kind === "dir" ? "Directory" : "File"})?`,
      )
    ) {
      return;
    }
    try {
      if (!isLocal && sessionId) {
        await remoteRemove(sessionId, entry.path, entry.kind === "dir");
      }
      // Close open tab if it matches this file
      const matchingTab = tabs.find((t) => t.path === entry.path);
      if (matchingTab) {
        handleCloseTab(matchingTab.id);
      }
      await loadDirectory(explorerPath);
    } catch (err) {
      alert(`Failed to delete: ${String(err)}`);
    }
  };

  const handleEditorMount: OnMount = (editor, monacoInstance) => {
    editorRef.current = editor;

    // Track cursor position
    editor.onDidChangeCursorPosition((e) => {
      setCursorPos({
        line: e.position.lineNumber,
        col: e.position.column,
      });
    });

    // Add keyboard shortcuts
    editor.addCommand(monacoInstance.KeyMod.CtrlCmd | monacoInstance.KeyCode.KeyS, () => {
      void handleSave();
    });

    editor.focus();
  };

  const handleFormat = () => {
    if (!editorRef.current) return;
    editorRef.current.getAction("editor.action.formatDocument")?.run();
  };

  const jumpToPosition = useCallback((line: number, col: number = 1) => {
    if (!editorRef.current) return;
    editorRef.current.revealLineInCenter(line);
    editorRef.current.setPosition({ lineNumber: line, column: col });
    editorRef.current.focus();
  }, []);

  const handleRunSearch = useCallback(async () => {
    if (!searchQuery.trim()) return;
    setSearchLoading(true);
    setSearchError(null);
    setSearchedOnce(true);

    const rootDir = searchTargetDir.trim() || explorerPath || "/";

    const options: SearchOptions = {
      query: searchQuery.trim(),
      case_sensitive: searchCaseSensitive,
      is_regex: searchRegex,
      whole_word: searchWholeWord,
      include_pattern: searchIncludePattern.trim() || null,
      max_results: 300,
      max_depth: 10,
    };

    try {
      let results: FileSearchResult[];
      if (isLocal || !sessionId) {
        results = await searchLocalDir(rootDir, options);
      } else {
        results = await searchRemoteDir(sessionId, rootDir, options);
      }
      setSearchResults(results);
    } catch (err) {
      setSearchError(String(err));
      setSearchResults([]);
    } finally {
      setSearchLoading(false);
    }
  }, [
    searchQuery,
    searchCaseSensitive,
    searchRegex,
    searchWholeWord,
    searchIncludePattern,
    searchTargetDir,
    explorerPath,
    isLocal,
    sessionId,
  ]);

  const handleMatchClick = async (fileResult: FileSearchResult, match: SearchMatch) => {
    const fileName = fileResult.path.split("/").pop() || fileResult.path;
    await openFile({
      path: fileResult.path,
      name: fileName,
      sessionId,
      hostLabel,
      isLocal,
    });
    setTimeout(() => {
      jumpToPosition(match.line_number, match.match_start + 1);
    }, 60);
  };

  const toggleFileCollapse = (filePath: string) => {
    setCollapsedFiles((prev) => ({ ...prev, [filePath]: !prev[filePath] }));
  };

  // Global keyboard shortcuts (Cmd/Ctrl+Shift+F for Search, Cmd/Ctrl+Shift+E for Explorer)
  useEffect(() => {
    const handleGlobalKeyDown = (e: KeyboardEvent) => {
      if (!open) return;
      if ((e.ctrlKey || e.metaKey) && e.shiftKey && (e.key === "F" || e.key === "f")) {
        e.preventDefault();
        setSidebarOpen(true);
        setSidebarTab("search");
      } else if ((e.ctrlKey || e.metaKey) && e.shiftKey && (e.key === "E" || e.key === "e")) {
        e.preventDefault();
        setSidebarOpen(true);
        setSidebarTab("explorer");
      }
    };
    window.addEventListener("keydown", handleGlobalKeyDown);
    return () => window.removeEventListener("keydown", handleGlobalKeyDown);
  }, [open]);

  // Filter and sort directory entries
  const filteredEntries = useMemo(() => {
    if (!listing) return [];
    let list = listing.entries;
    if (!showHidden) {
      list = list.filter((e) => !e.hidden);
    }
    if (searchFilter.trim()) {
      const q = searchFilter.trim().toLowerCase();
      list = list.filter((e) => e.name.toLowerCase().includes(q));
    }
    // Dirs first, then files alphabetically
    return [...list].sort((a, b) => {
      if (a.kind === "dir" && b.kind !== "dir") return -1;
      if (a.kind !== "dir" && b.kind === "dir") return 1;
      return a.name.localeCompare(b.name, undefined, { sensitivity: "base" });
    });
  }, [listing, showHidden, searchFilter]);

  if (!open) return null;

  const totalLines = activeTab ? activeTab.content.split("\n").length : 0;
  const totalChars = activeTab ? activeTab.content.length : 0;

  return (
    <div className="editor-modal-backdrop" onClick={onClose}>
      <div
        className={`editor-window ${fullscreen ? "fullscreen" : ""}`}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header Bar */}
        <header className="editor-header">
          <div className="editor-header-left">
            <span className="editor-badge">📝 Mini-IDE</span>
            <button
              className={`editor-btn icon-only ${sidebarOpen && sidebarTab === "explorer" ? "active" : ""}`}
              onClick={() => {
                if (sidebarOpen && sidebarTab === "explorer") {
                  setSidebarOpen(false);
                } else {
                  setSidebarOpen(true);
                  setSidebarTab("explorer");
                }
              }}
              title="Toggle Project Explorer Sidebar (Ctrl+Shift+E)"
            >
              🗂️ Explorer
            </button>
            <button
              className={`editor-btn icon-only ${sidebarOpen && sidebarTab === "search" ? "active" : ""}`}
              onClick={() => {
                if (sidebarOpen && sidebarTab === "search") {
                  setSidebarOpen(false);
                } else {
                  setSidebarOpen(true);
                  setSidebarTab("search");
                }
              }}
              title="Toggle Global Search in Folder (Ctrl+Shift+F)"
            >
              🔍 Search
            </button>
            {(activeTab?.hostLabel || hostLabel) && (
              <span className="editor-host-tag">
                {activeTab?.hostLabel || hostLabel}
              </span>
            )}
            <div className="editor-path-input-wrapper">
              <input
                className="editor-path-input"
                value={pathInput}
                onChange={(e) => setPathInput(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") handleOpenPathFromInput();
                }}
                placeholder="Enter remote file path (/path/to/file.py)..."
              />
            </div>
            <button
              className="editor-btn"
              onClick={handleOpenPathFromInput}
              title="Open path in editor"
            >
              Open
            </button>
          </div>

          <div className="editor-header-actions">
            {saveStatus && (
              <span
                style={{
                  fontSize: 12,
                  fontFamily: "var(--mono)",
                  color: saveStatus.includes("failed") ? "var(--danger)" : "var(--lime)",
                  marginRight: 6,
                }}
              >
                {saveStatus}
              </span>
            )}

            <button
              className="editor-btn primary"
              onClick={() => void handleSave()}
              disabled={!activeTab || activeTab.loading}
              title="Save to remote host (⌘S / Ctrl+S)"
            >
              💾 Save
            </button>

            <button
              className="editor-btn"
              onClick={() => void handleReload()}
              disabled={!activeTab || activeTab.loading}
              title="Reload content from server"
            >
              ⟳ Reload
            </button>

            <button
              className="editor-btn icon-only"
              onClick={handleFormat}
              disabled={!activeTab || activeTab.loading}
              title="Format Document (Shift+Alt+F)"
            >
              🪄 Format
            </button>

            <button
              className={`editor-btn icon-only ${wordWrap ? "active" : ""}`}
              onClick={() => setWordWrap((w) => !w)}
              title="Toggle Word Wrap"
            >
              Wrap
            </button>

            <button
              className={`editor-btn icon-only ${minimap ? "active" : ""}`}
              onClick={() => setMinimap((m) => !m)}
              title="Toggle Minimap"
            >
              Map
            </button>

            <button
              className="editor-btn icon-only"
              onClick={() => setFontSize((s) => Math.max(10, s - 1))}
              title="Decrease Font Size"
            >
              A-
            </button>

            <button
              className="editor-btn icon-only"
              onClick={() => setFontSize((s) => Math.min(24, s + 1))}
              title="Increase Font Size"
            >
              A+
            </button>

            <button
              className="editor-btn icon-only"
              onClick={() => setFullscreen((f) => !f)}
              title={fullscreen ? "Restore Size" : "Maximize / Fullscreen"}
            >
              {fullscreen ? "🗗" : "🗖"}
            </button>

            <button className="editor-btn icon-only" onClick={onClose} title="Close Editor">
              ✕
            </button>
          </div>
        </header>

        {/* Main Content Area (Sidebar Explorer + Editor Tabs & Body) */}
        <div className="editor-main-container">
          {/* Project Explorer & Global Search Sidebar */}
          {sidebarOpen && (
            <aside className="editor-sidebar">
              {/* Sidebar View Switcher Tabs */}
              <div className="editor-sidebar-tabs">
                <button
                  className={`editor-sidebar-tab-btn ${sidebarTab === "explorer" ? "active" : ""}`}
                  onClick={() => setSidebarTab("explorer")}
                  title="Filesystem Explorer (Ctrl+Shift+E)"
                >
                  📁 Explorer
                </button>
                <button
                  className={`editor-sidebar-tab-btn ${sidebarTab === "search" ? "active" : ""}`}
                  onClick={() => setSidebarTab("search")}
                  title="Global Search in Directory (Ctrl+Shift+F)"
                >
                  🔍 Search
                </button>
              </div>

              {/* View 1: Explorer Panel */}
              {sidebarTab === "explorer" && (
                <>
                  <div className="editor-sidebar-header">
                    <span className="editor-sidebar-title">
                      <span>📂 Files</span>
                    </span>
                    <div className="editor-sidebar-actions">
                      <button
                        className="editor-sidebar-action-btn"
                        onClick={() => {
                          setNewItem({ type: "file" });
                          setNewItemName("");
                        }}
                        title="New File in Current Folder"
                      >
                        📄+
                      </button>
                      <button
                        className="editor-sidebar-action-btn"
                        onClick={() => {
                          setNewItem({ type: "dir" });
                          setNewItemName("");
                        }}
                        title="New Folder in Current Folder"
                      >
                        📁+
                      </button>
                      <button
                        className="editor-sidebar-action-btn"
                        onClick={() => void loadDirectory(explorerPath || "/")}
                        title="Refresh Directory"
                      >
                        🔄
                      </button>
                      <button
                        className={`editor-sidebar-action-btn ${showHidden ? "active" : ""}`}
                        onClick={() => setShowHidden((v) => !v)}
                        title={showHidden ? "Hide Hidden Dotfiles" : "Show Hidden Dotfiles"}
                      >
                        👁️
                      </button>
                      {listing?.parent && (
                        <button
                          className="editor-sidebar-action-btn"
                          onClick={() => void loadDirectory(listing.parent!)}
                          title="Go to Parent Folder"
                        >
                          ⬆️
                        </button>
                      )}
                    </div>
                  </div>

                  {/* Location Path Row */}
                  <div className="editor-sidebar-location">
                    <div className="editor-sidebar-path-row">
                      <input
                        className="editor-sidebar-path-input"
                        value={explorerPathInput}
                        onChange={(e) => setExplorerPathInput(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") void loadDirectory(explorerPathInput);
                        }}
                        placeholder="/path/to/folder..."
                      />
                      <button
                        className="editor-sidebar-action-btn"
                        onClick={() => void loadDirectory(explorerPathInput)}
                        title="Navigate to path"
                      >
                        ➔
                      </button>
                    </div>
                    {/* Quick Jumps */}
                    <div className="editor-sidebar-quick-jumps">
                      <button
                        className="editor-quick-jump-chip"
                        onClick={async () => {
                          try {
                            const h = isLocal || !sessionId ? await localHome() : await remoteHome(sessionId);
                            void loadDirectory(h);
                          } catch {
                            void loadDirectory("/");
                          }
                        }}
                        title="Jump to Home Directory"
                      >
                        ~ home
                      </button>
                      <button
                        className="editor-quick-jump-chip"
                        onClick={() => void loadDirectory("/")}
                        title="Jump to Root /"
                      >
                        / root
                      </button>
                      <button
                        className="editor-quick-jump-chip"
                        onClick={() => void loadDirectory("/etc")}
                        title="Jump to /etc"
                      >
                        /etc
                      </button>
                      <button
                        className="editor-quick-jump-chip"
                        onClick={() => void loadDirectory("/var/log")}
                        title="Jump to /var/log"
                      >
                        /var/log
                      </button>
                      <button
                        className="editor-quick-jump-chip"
                        onClick={() => void loadDirectory("/tmp")}
                        title="Jump to /tmp"
                      >
                        /tmp
                      </button>
                    </div>
                  </div>

                  {/* Live Search in folder */}
                  <div className="editor-sidebar-search">
                    <input
                      className="editor-sidebar-search-input"
                      value={searchFilter}
                      onChange={(e) => setSearchFilter(e.target.value)}
                      placeholder="Filter files in current folder..."
                    />
                  </div>

                  {/* Inline Create Row */}
                  {newItem && (
                    <div className="editor-sidebar-new-input-box">
                      <span style={{ fontSize: 13 }}>{newItem.type === "file" ? "📄" : "📁"}</span>
                      <input
                        className="editor-sidebar-new-input"
                        autoFocus
                        value={newItemName}
                        onChange={(e) => setNewItemName(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") void handleCreateNewItem();
                          if (e.key === "Escape") setNewItem(null);
                        }}
                        placeholder={`Name of new ${newItem.type}...`}
                      />
                      <button
                        className="editor-sidebar-action-btn"
                        onClick={() => void handleCreateNewItem()}
                        title="Create"
                      >
                        ✓
                      </button>
                      <button
                        className="editor-sidebar-action-btn"
                        onClick={() => setNewItem(null)}
                        title="Cancel"
                      >
                        ✕
                      </button>
                    </div>
                  )}

                  {/* Directory Tree / List */}
                  <div className="editor-sidebar-tree">
                    {loadingDir && (
                      <div className="editor-sidebar-loading">
                        <div className="editor-spinner" />
                        <span>Loading directory...</span>
                      </div>
                    )}

                    {dirError && !loadingDir && (
                      <div className="editor-sidebar-empty">
                        <span style={{ color: "var(--danger)" }}>⚠️ {dirError}</span>
                        <button
                          className="editor-btn"
                          style={{ marginTop: 6 }}
                          onClick={() => void loadDirectory(explorerPath || "/")}
                        >
                          Retry
                        </button>
                      </div>
                    )}

                    {!loadingDir && !dirError && (
                      <>
                        {/* Up to Parent Directory item */}
                        {listing?.parent && (
                          <div
                            className="editor-tree-item dir"
                            onClick={() => void loadDirectory(listing.parent!)}
                            title="Go to parent directory"
                          >
                            <div className="editor-tree-item-left">
                              <span className="editor-tree-icon">📁</span>
                              <span className="editor-tree-name">.. (parent)</span>
                            </div>
                          </div>
                        )}

                        {filteredEntries.map((entry) => {
                          const isDir = entry.kind === "dir";
                          const isOpenedInActive = activeTab?.path === entry.path;
                          return (
                            <div
                              key={entry.path}
                              className={`editor-tree-item ${isDir ? "dir" : "file"} ${entry.hidden ? "hidden-file" : ""} ${isOpenedInActive ? "active" : ""}`}
                              onClick={() => {
                                if (isDir) {
                                  void loadDirectory(entry.path);
                                } else {
                                  void openFile({
                                    path: entry.path,
                                    name: entry.name,
                                    sessionId,
                                    hostLabel,
                                    isLocal,
                                  });
                                }
                              }}
                              title={entry.path}
                            >
                              <div className="editor-tree-item-left">
                                <span className="editor-tree-icon">
                                  {isDir ? "📁" : getFileIcon(entry.name)}
                                </span>
                                <span className="editor-tree-name">{entry.name}</span>
                              </div>

                              <div className="editor-tree-item-right">
                                {!isDir && (
                                  <span className="editor-tree-size">
                                    {formatFileSize(entry.size)}
                                  </span>
                                )}
                                <div className="editor-tree-actions">
                                  <button
                                    className="editor-tree-action-btn"
                                    onClick={(e) => void handleDeleteEntry(entry, e)}
                                    title={`Delete ${entry.name}`}
                                  >
                                    🗑️
                                  </button>
                                </div>
                              </div>
                            </div>
                          );
                        })}

                        {filteredEntries.length === 0 && !listing?.parent && (
                          <div className="editor-sidebar-empty">
                            <span>Empty folder</span>
                          </div>
                        )}
                      </>
                    )}
                  </div>
                </>
              )}

              {/* View 2: Global Directory Search Panel */}
              {sidebarTab === "search" && (
                <div className="editor-search-panel">
                  <div className="editor-search-form">
                    {/* Search Input Box */}
                    <div className="editor-search-input-box">
                      <input
                        className="editor-search-text-input"
                        value={searchQuery}
                        onChange={(e) => setSearchQuery(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") void handleRunSearch();
                        }}
                        placeholder="Search text in files..."
                        autoFocus
                      />
                      <button
                        className={`editor-search-toggle-btn ${searchCaseSensitive ? "active" : ""}`}
                        onClick={() => setSearchCaseSensitive((v) => !v)}
                        title="Match Case (Aa)"
                      >
                        Aa
                      </button>
                      <button
                        className={`editor-search-toggle-btn ${searchWholeWord ? "active" : ""}`}
                        onClick={() => setSearchWholeWord((v) => !v)}
                        title="Match Whole Word (\b)"
                      >
                        \b
                      </button>
                      <button
                        className={`editor-search-toggle-btn ${searchRegex ? "active" : ""}`}
                        onClick={() => setSearchRegex((v) => !v)}
                        title="Use Regular Expression (.*)"
                      >
                        .*
                      </button>
                    </div>

                    {/* Files to Include Glob */}
                    <div className="editor-search-options-row">
                      <input
                        className="editor-search-filter-input"
                        value={searchIncludePattern}
                        onChange={(e) => setSearchIncludePattern(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") void handleRunSearch();
                        }}
                        placeholder="Include (e.g. *.py, *.rs, src/*)"
                        title="File filter pattern"
                      />
                    </div>

                    {/* Search Target Root Directory */}
                    <div className="editor-search-options-row">
                      <input
                        className="editor-search-filter-input"
                        value={searchTargetDir || explorerPath}
                        onChange={(e) => setSearchTargetDir(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") void handleRunSearch();
                        }}
                        placeholder="Search folder (default: current)"
                        title="Directory path to search recursively"
                      />
                    </div>

                    {/* Search Action Buttons */}
                    <div className="editor-search-actions-row">
                      <button
                        className="editor-btn primary"
                        style={{ padding: "3px 12px", fontSize: 11 }}
                        onClick={() => void handleRunSearch()}
                        disabled={searchLoading || !searchQuery.trim()}
                      >
                        {searchLoading ? "Searching..." : "🔍 Find"}
                      </button>

                      {searchResults.length > 0 && (
                        <button
                          className="editor-btn"
                          style={{ padding: "3px 8px", fontSize: 11 }}
                          onClick={() => {
                            setSearchResults([]);
                            setSearchedOnce(false);
                            setSearchError(null);
                          }}
                        >
                          Clear
                        </button>
                      )}
                    </div>
                  </div>

                  {/* Results Summary */}
                  {searchedOnce && !searchLoading && (
                    <div className="editor-search-results-summary">
                      <span>
                        {searchResults.length === 0
                          ? "No matches found"
                          : `${searchResults.reduce((a, b) => a + b.matches.length, 0)} results in ${searchResults.length} files`}
                      </span>
                    </div>
                  )}

                  {/* Results Tree */}
                  <div className="editor-search-results-tree">
                    {searchLoading && (
                      <div className="editor-sidebar-loading">
                        <div className="editor-spinner" />
                        <span>Searching directory tree...</span>
                      </div>
                    )}

                    {searchError && (
                      <div className="editor-sidebar-empty" style={{ color: "var(--danger)" }}>
                        <span>⚠️ {searchError}</span>
                      </div>
                    )}

                    {!searchLoading &&
                      searchResults.map((fileRes) => {
                        const isCollapsed = !!collapsedFiles[fileRes.path];
                        const fileName = fileRes.path.split("/").pop() || fileRes.path;
                        const icon = getFileIcon(fileName);

                        return (
                          <div key={fileRes.path} className="editor-search-file-group">
                            <div
                              className="editor-search-file-header"
                              onClick={() => toggleFileCollapse(fileRes.path)}
                              title={fileRes.path}
                            >
                              <div className="editor-search-file-title">
                                <span style={{ fontSize: 10, color: "var(--dim)" }}>
                                  {isCollapsed ? "▶" : "▼"}
                                </span>
                                <span>{icon}</span>
                                <span style={{ fontWeight: 600 }}>{fileName}</span>
                                <span className="editor-search-file-path">
                                  {fileRes.relative_path !== fileName ? `(${fileRes.relative_path})` : ""}
                                </span>
                              </div>
                              <span className="editor-search-badge">
                                {fileRes.matches.length}
                              </span>
                            </div>

                            {!isCollapsed &&
                              fileRes.matches.map((match, mIdx) => (
                                <div
                                  key={`${fileRes.path}-${match.line_number}-${mIdx}`}
                                  className="editor-search-match-row"
                                  onClick={() => void handleMatchClick(fileRes, match)}
                                  title={`Go to line ${match.line_number}`}
                                >
                                  <span className="editor-search-line-no">
                                    L{match.line_number}
                                  </span>
                                  {renderMatchSnippet(match)}
                                </div>
                              ))}
                          </div>
                        );
                      })}

                    {!searchLoading && !searchedOnce && (
                      <div className="editor-sidebar-empty">
                        <span style={{ fontSize: 24, opacity: 0.5 }}>🔍</span>
                        <span>Enter text and click Find to search folder</span>
                      </div>
                    )}
                  </div>
                </div>
              )}
            </aside>
          )}

          {/* Right Content Pane: Tabs + Monaco Canvas + Statusbar */}
          <main className="editor-content-pane">
            {/* Tab Bar */}
            <div className="editor-tabs-bar">
              {tabs.map((tab) => (
                <div
                  key={tab.id}
                  className={`editor-tab ${tab.id === activeTabId ? "active" : ""}`}
                  onClick={() => {
                    setActiveTabId(tab.id);
                    setPathInput(tab.path);
                  }}
                  title={tab.path}
                >
                  <span className="editor-tab-icon">{getFileIcon(tab.name)}</span>
                  <span className="editor-tab-name">{tab.name}</span>
                  {tab.isDirty && <span className="editor-tab-dirty" title="Unsaved changes" />}
                  <span
                    className="editor-tab-close"
                    onClick={(e) => handleCloseTab(tab.id, e)}
                    title="Close tab"
                  >
                    ×
                  </span>
                </div>
              ))}
              <button className="editor-new-tab-btn" onClick={handleNewTab} title="New file tab">
                +
              </button>
            </div>

            {/* Editor Area */}
            <div className="editor-body">
              {activeTab ? (
                <>
                  {activeTab.loading && (
                    <div className="editor-loading-overlay">
                      <div className="editor-spinner" />
                      <span>Loading file {activeTab.path}...</span>
                    </div>
                  )}
                  {activeTab.error && (
                    <div className="editor-empty-state">
                      <span className="editor-empty-icon">⚠️</span>
                      <div style={{ color: "var(--danger)", fontWeight: 600 }}>
                        Failed to load file
                      </div>
                      <div style={{ color: "var(--muted)", maxWidth: 500 }}>
                        {activeTab.error}
                      </div>
                      <button className="editor-btn" onClick={() => void handleReload()}>
                        Retry
                      </button>
                    </div>
                  )}
                  {!activeTab.error && (
                    <Editor
                      height="100%"
                      language={activeTab.language}
                      value={activeTab.content}
                      theme="terminator-dark"
                      beforeMount={handleEditorWillMount}
                      onMount={handleEditorMount}
                      onChange={handleContentChange}
                      options={{
                        fontFamily:
                          '"JetBrains Mono", ui-monospace, Menlo, Consolas, monospace',
                        fontSize,
                        lineHeight: 1.4,
                        wordWrap: wordWrap ? "on" : "off",
                        minimap: { enabled: minimap },
                        scrollBeyondLastLine: false,
                        smoothScrolling: true,
                        cursorBlinking: "smooth",
                        cursorSmoothCaretAnimation: "on",
                        bracketPairColorization: { enabled: true },
                        autoClosingBrackets: "always",
                        autoClosingQuotes: "always",
                        formatOnPaste: true,
                        formatOnType: true,
                        renderWhitespace: "selection",
                        tabSize: 4,
                      }}
                    />
                  )}
                </>
              ) : (
                <div className="editor-empty-state">
                  <span className="editor-empty-icon">📂</span>
                  <div style={{ fontSize: 16, fontWeight: 600, color: "var(--fg)" }}>
                    No file open
                  </div>
                  <div style={{ color: "var(--muted)", maxWidth: 450 }}>
                    Click any file in the <strong>Project Explorer sidebar</strong> on the left,
                    or click <strong>+</strong> to start editing a new file.
                  </div>
                  <div style={{ display: "flex", gap: 10 }}>
                    <button className="editor-btn primary" onClick={handleNewTab}>
                      + New Untitled File
                    </button>
                  </div>
                </div>
              )}
            </div>

            {/* Status Bar */}
            <footer className="editor-statusbar">
              <div className="editor-statusbar-left">
                <span className="editor-status-item">
                  Ln {cursorPos.line}, Col {cursorPos.col}
                </span>
                <span className="editor-status-item">
                  {totalLines} lines, {totalChars} chars
                </span>
                {activeTab?.isDirty && (
                  <span className="editor-status-item" style={{ color: "var(--lime)" }}>
                    ● Unsaved changes
                  </span>
                )}
              </div>

              <div className="editor-statusbar-right">
                <span className="editor-status-item">UTF-8</span>
                <span className="editor-status-item">Spaces: 4</span>
                <select
                  className="editor-select"
                  value={activeTab?.language || "plaintext"}
                  onChange={(e) => {
                    const newLang = e.target.value;
                    if (!activeTabId) return;
                    setTabs((prev) =>
                      prev.map((t) =>
                        t.id === activeTabId ? { ...t, language: newLang } : t,
                      ),
                    );
                  }}
                  title="Select syntax language mode"
                >
                  {SUPPORTED_LANGUAGES.map((lang) => (
                    <option key={lang.id} value={lang.id}>
                      {lang.label}
                    </option>
                  ))}
                </select>
              </div>
            </footer>
          </main>
        </div>
      </div>
    </div>
  );
}
