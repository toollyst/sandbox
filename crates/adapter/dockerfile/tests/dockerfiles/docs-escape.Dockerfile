# escape=`

FROM mcr.microsoft.com/windows/servercore
COPY testfile.txt c:\
RUN dir c:\
