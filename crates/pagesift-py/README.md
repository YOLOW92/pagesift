# pagesift (Python)

Python bindings for [pagesift](https://github.com/YOLOW92/pagesift). Early development.

```python
import pagesift

doc = pagesift.convert(html, url="https://example.com/post")
print(doc.markdown)
print(doc.metadata["title"])
```

`convert` releases the GIL, so a plain `ThreadPoolExecutor` uses every core.
