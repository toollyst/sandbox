FROM busybox
ENV FOO=/bar
WORKDIR ${FOO}
ADD . $FOO
COPY \$FOO /quux
ENV abc=hello
ENV abc=bye def=$abc
ENV ghi=$abc
ENV MY_NAME="John Doe"
ENV MY_DOG=Rex\ The\ Dog
ENV MY_CAT=fluffy
ENV MY_VAR my-value
LABEL "com.example.vendor"="ACME Incorporated"
LABEL com.example.label-with-value="foo"
LABEL version="1.0"
LABEL description="This text illustrates \
that label-values can span multiple lines."
LABEL multi.label1="value1" multi.label2="value2" other="value3"
