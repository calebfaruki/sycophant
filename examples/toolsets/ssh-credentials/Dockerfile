ARG BASE_IMAGE=toolset-base:local
FROM ${BASE_IMAGE}
LABEL md.sycophant.tools='[\
  {"name": "test-cmd", "description": "Run the toolset self-test and return its output. Used by integration tests to verify the toolset execution and result-handling pipeline.", "args": {}},\
  {"name": "test-cred", "description": "Read the credential at the default grant path and return its bytes. Used by integration tests to verify pathless grant delivery.", "args": {}}\
]'
COPY dispatch /etc/toolset/dispatch
RUN chmod +x /etc/toolset/dispatch
