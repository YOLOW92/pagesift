"""单核计时的公平条件：benchmarks/common/fresh/pinned.sh 设 PAGESIFT_ONE_CORE=1 时，让 Python 工具只用分到的那个核。

taskset 只能约束进程起始的亲和性，onnxruntime（pymupdf4llm 的版面模型、MarkItDown 里的 Magika）会给自己的
工作线程逐个重设亲和性，跑到别的物理核上去。这里在导入时把 onnxruntime 的会话改成 1 个算子内线程、1 个算子间
线程、不自旋，它就不会再开线程池。BLAS/OpenMP 的线程数由 pinned.sh 用环境变量设成 1。

安装：benchmarks/common/fresh/one_core.py install <venv 的 python>（把本文件和一个 .pth 钩子放进该 venv 的 site-packages）。
"""
import os
import sys


def _patch():
    try:
        import onnxruntime as ort
    except Exception:
        return
    orig = ort.InferenceSession.__init__

    def init(self, path_or_bytes, sess_options=None, *args, **kwargs):
        so = sess_options or ort.SessionOptions()
        so.intra_op_num_threads = 1
        so.inter_op_num_threads = 1
        so.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
        try:
            so.add_session_config_entry("session.intra_op.allow_spinning", "0")
            so.add_session_config_entry("session.inter_op.allow_spinning", "0")
        except Exception:
            pass
        orig(self, path_or_bytes, so, *args, **kwargs)

    ort.InferenceSession.__init__ = init


if os.environ.get("PAGESIFT_ONE_CORE") == "1":
    _patch()

if __name__ == "__main__" and sys.argv[1:2] == ["install"]:
    import shutil
    import subprocess
    site = subprocess.run([sys.argv[2], "-c", "import sysconfig; print(sysconfig.get_paths()['purelib'])"],
                          capture_output=True, text=True, check=True).stdout.strip()
    shutil.copy(__file__, os.path.join(site, "pagesift_one_core.py"))
    with open(os.path.join(site, "pagesift_one_core.pth"), "w") as f:
        f.write("import pagesift_one_core\n")
    print("installed into", site)
