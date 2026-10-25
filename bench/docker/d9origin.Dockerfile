FROM python:3.12-slim
WORKDIR /app
COPY d9_origin.py /app/d9_origin.py
EXPOSE 8200
CMD ["python", "/app/d9_origin.py", "8200"]
