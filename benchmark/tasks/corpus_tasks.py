from tasks.base import GroundTruth, Task


class TypeScriptExpressMiddlewareLocateTask(Task):
    capability = "locate"

    @property
    def name(self) -> str:
        return "ts_express_middleware_locate"

    @property
    def repo(self) -> str:
        return "synthetic"

    @property
    def prompt(self) -> str:
        return (
            "In src/typescript/express_middleware.ts, locate the exported "
            "`createAuthMiddleware` factory and the `app.use` registration that "
            "installs it. Name both symbols and give their file path."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "express_middleware.ts",
                "createAuthMiddleware",
                "app.use",
            ],
        )


class TypeScriptGenericTraceTask(Task):
    capability = "trace"

    @property
    def name(self) -> str:
        return "ts_generic_trace"

    @property
    def repo(self) -> str:
        return "synthetic"

    @property
    def prompt(self) -> str:
        return (
            "In src/typescript/generic_trace.ts, trace `loadUser` through "
            "`GenericRepository<UserRecord>`. Identify the generic class, its "
            "`findById` method, and the caller's type specialization."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "generic_trace.ts",
                "GenericRepository",
                "findById",
                "loadUser",
            ],
        )


class JavaLocateImplementorTask(Task):
    capability = "locate"

    @property
    def name(self) -> str:
        return "java_locate_implementor"

    @property
    def repo(self) -> str:
        return "synthetic"

    @property
    def prompt(self) -> str:
        return (
            "In src/java/Implementor.java, locate the `Notifier` interface and "
            "list every class that implements it. Include the implementing class "
            "names and file path."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "Implementor.java",
                "Notifier",
                "implements Notifier",
                "EmailNotifier",
                "WebhookNotifier",
            ],
        )


class GrokRipgrepMatcherTask(Task):
    capability = "trace"

    @property
    def name(self) -> str:
        return "grok_rg_matcher"

    @property
    def repo(self) -> str:
        return "ripgrep"

    @property
    def prompt(self) -> str:
        return (
            "Give me a complete picture of ripgrep's `RegexMatcher`: locate its "
            "definition in `crates/regex/src/matcher.rs`, explain how it implements "
            "`Matcher::find_at`, and identify callers. One structured answer is "
            "better than several partial searches."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "crates/regex/src/matcher.rs",
                "RegexMatcher",
                "impl Matcher for RegexMatcher",
                "find_at",
            ],
        )

    @property
    def task_type(self) -> str:
        return "navigate"


class FastAPIDependencyTargetTask(Task):
    capability = "locate"

    @property
    def name(self) -> str:
        return "fapi_deps_target"

    @property
    def repo(self) -> str:
        return "fastapi"

    @property
    def prompt(self) -> str:
        return (
            "In fastapi/dependencies, identify the module or modules that import from "
            "fastapi._compat. Give the importing file paths and the imported names."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "dependencies/models.py",
                "dependencies/utils.py",
                "ModelField",
                "RequiredParam",
            ]
        )


class GinDependencyTargetTask(Task):
    capability = "locate"

    @property
    def name(self) -> str:
        return "gin_deps_target"

    @property
    def repo(self) -> str:
        return "gin"

    @property
    def prompt(self) -> str:
        return (
            "In gin.go, identify what it imports from internal/bytesconv. Show the "
            "import path and the names or helpers used from that package."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "github.com/gin-gonic/gin/internal/bytesconv",
                "BytesToString",
            ]
        )


class ControlPackageManifestTask(Task):
    capability = "control"

    @property
    def name(self) -> str:
        return "control_pkg_manifest"

    @property
    def repo(self) -> str:
        return "synthetic"

    @property
    def prompt(self) -> str:
        return "Read pyproject.toml and list the project name, Python requirement, and runtime dependencies."

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "flask-api",
                ">=3.9",
                "flask>=2.3.0",
                "pyjwt>=2.8.0",
                "psycopg2-binary>=2.9.0",
                "pytest>=7.4.0",
            ]
        )


class ControlChangelogReadTask(Task):
    capability = "control"

    @property
    def name(self) -> str:
        return "control_changelog_read"

    @property
    def repo(self) -> str:
        return "synthetic"

    @property
    def prompt(self) -> str:
        return "Read CHANGELOG.md and summarize the Added and Fixed entries in the current release."

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(
            required_strings=[
                "CHANGELOG.md",
                "1.0.0",
                "request validation helpers",
                "database connection pool annotations",
            ]
        )


class ControlArithmeticTask(Task):
    capability = "control"

    @property
    def name(self) -> str:
        return "control_arith_util"

    @property
    def repo(self) -> str:
        return "synthetic"

    @property
    def prompt(self) -> str:
        return (
            "Read src/utils/arithmetic.py and calculate the documented result of "
            "combine(6, 7). State the two operations used and the final value."
        )

    @property
    def ground_truth(self) -> GroundTruth:
        return GroundTruth(required_strings=["arithmetic.py", "combine", "42"])
