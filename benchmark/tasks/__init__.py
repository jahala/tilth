from .find_definition import FindDefinitionTask
from .read_large_file import ReadLargeFileTask
from .edit_task import EditTask
from .codebase_navigation import CodebaseNavigationTask
from .markdown_section import MarkdownSectionTask
from .ripgrep_tasks import (
    RipgrepTraitImplementorsTask,
    RipgrepFlagDefinitionTask,
    RipgrepSearchDispatchTask,
    RipgrepWalkerParallelTask,
    RipgrepLineIterDefinitionTask,
    RipgrepLineIterUsageTask,
)
from .ripgrep_edit_tasks import (
    RipgrepEditLineCountTask,
    RipgrepEditLineLocateTask,
    RipgrepEditPrecedingLinesTask,
)
from .fastapi_tasks import (
    FastAPIDependencyResolutionTask,
    FastAPIRequestValidationTask,
    FastAPIDependsInternalsTask,
    FastAPIDependsFunctionTask,
    FastAPIDependsProcessingTask,
)
from .fastapi_edit_tasks import (
    FastAPIEditDepCacheTask,
    FastAPIEditResponseFilterTask,
    FastAPIEditScopeCacheTask,
)
from .gin_tasks import (
    GinRadixTreeTask,
    GinClientIPTask,
    GinMiddlewareChainTask,
    GinContextNextTask,
    GinServeHTTPFlowTask,
)
from .gin_edit_tasks import (
    GinEditMiddlewareChainTask,
    GinEditAbortCheckTask,
    GinEditContextResetTask,
)
from .express_tasks import (
    ExpressJsonSendTask,
    ExpressRenderChainTask,
    ExpressAppInitTask,
    ExpressResSendTask,
    ExpressAppRenderTask,
)
from .express_edit_tasks import (
    ExpressEditJsonContentTypeTask,
    ExpressEditCookiePrefixTask,
    ExpressEditSendHtmlTypeTask,
)
from .express_diff_tasks import ExpressDiffMultiMutationTask
from .fastapi_diff_tasks import FastAPIDiffWhichCommitTask
from .ripgrep_diff_tasks import RipgrepDiffMisdirectedErrorTask
from .gin_diff_tasks import GinDiffComprehensionTask

TASKS = {
    # Synthetic repo tasks
    "find_definition": FindDefinitionTask(),
    "read_large_file": ReadLargeFileTask(),
    "edit_task": EditTask(),
    "codebase_navigation": CodebaseNavigationTask(),
    "markdown_section": MarkdownSectionTask(),
    # ripgrep (Rust)
    "rg_trait_implementors": RipgrepTraitImplementorsTask(),
    "rg_flag_definition": RipgrepFlagDefinitionTask(),
    "rg_search_dispatch": RipgrepSearchDispatchTask(),
    "rg_walker_parallel": RipgrepWalkerParallelTask(),
    "rg_lineiter_definition": RipgrepLineIterDefinitionTask(),
    "rg_lineiter_usage": RipgrepLineIterUsageTask(),
    "rg_edit_line_count": RipgrepEditLineCountTask(),
    "rg_edit_line_locate": RipgrepEditLineLocateTask(),
    "rg_edit_preceding": RipgrepEditPrecedingLinesTask(),
    # fastapi (Python)
    "fastapi_dependency_resolution": FastAPIDependencyResolutionTask(),
    "fastapi_request_validation": FastAPIRequestValidationTask(),
    "fastapi_depends_internals": FastAPIDependsInternalsTask(),
    "fastapi_depends_function": FastAPIDependsFunctionTask(),
    "fastapi_depends_processing": FastAPIDependsProcessingTask(),
    "fastapi_edit_dep_cache": FastAPIEditDepCacheTask(),
    "fastapi_edit_response_filter": FastAPIEditResponseFilterTask(),
    "fastapi_edit_scope_cache": FastAPIEditScopeCacheTask(),
    # gin (Go)
    "gin_radix_tree": GinRadixTreeTask(),
    "gin_client_ip": GinClientIPTask(),
    "gin_middleware_chain": GinMiddlewareChainTask(),
    "gin_context_next": GinContextNextTask(),
    "gin_servehttp_flow": GinServeHTTPFlowTask(),
    "gin_edit_middleware_skip": GinEditMiddlewareChainTask(),
    "gin_edit_abort_check": GinEditAbortCheckTask(),
    "gin_edit_context_reset": GinEditContextResetTask(),
    # express (JavaScript)
    "express_json_send": ExpressJsonSendTask(),
    "express_render_chain": ExpressRenderChainTask(),
    "express_app_init": ExpressAppInitTask(),
    "express_res_send": ExpressResSendTask(),
    "express_app_render": ExpressAppRenderTask(),
    "express_edit_json_type": ExpressEditJsonContentTypeTask(),
    "express_edit_cookie_prefix": ExpressEditCookiePrefixTask(),
    "express_edit_send_type": ExpressEditSendHtmlTypeTask(),
    # diff-specific benchmark tasks
    "express_diff_multi_mutation": ExpressDiffMultiMutationTask(),
    "fastapi_diff_which_commit": FastAPIDiffWhichCommitTask(),
    "rg_diff_misdirected_error": RipgrepDiffMisdirectedErrorTask(),
    "gin_diff_comprehension": GinDiffComprehensionTask(),
}
