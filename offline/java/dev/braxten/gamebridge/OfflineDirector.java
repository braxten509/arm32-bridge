package dev.braxten.gamebridge;

import java.io.IOException;
import java.net.UnknownHostException;
import org.apache.http.Header;
import org.apache.http.HttpEntity;
import org.apache.http.HttpEntityEnclosingRequest;
import org.apache.http.HttpException;
import org.apache.http.HttpHost;
import org.apache.http.HttpRequest;
import org.apache.http.HttpResponse;
import org.apache.http.HttpVersion;
import org.apache.http.client.RequestDirector;
import org.apache.http.entity.ByteArrayEntity;
import org.apache.http.message.BasicHttpResponse;
import org.apache.http.message.BasicStatusLine;
import org.apache.http.protocol.HttpContext;
import org.apache.http.util.EntityUtils;

/**
 * Hands one Apache HttpClient request to the bridge (Offline.answer) and turns its answer
 * into the response the game reads. No answer: UnknownHostException, what the game sees
 * when the phone has no network.
 */
public final class OfflineDirector implements RequestDirector {
    @Override
    public HttpResponse execute(HttpHost target, HttpRequest request, HttpContext context) throws HttpException, IOException {
        Header[] all = request.getAllHeaders();
        String[] headers = new String[all.length];
        for (int i = 0; i < all.length; i++) {
            headers[i] = all[i].getName() + ": " + all[i].getValue();
        }
        byte[] body = new byte[0];
        if (request instanceof HttpEntityEnclosingRequest) {
            HttpEntity entity = ((HttpEntityEnclosingRequest) request).getEntity();
            if (entity != null) {
                body = EntityUtils.toByteArray(entity);
            }
        }
        byte[] answer = Offline.answer(target.getSchemeName(), target.getHostName(), target.getPort(),
                request.getRequestLine().getMethod(), request.getRequestLine().getUri(), headers, body);
        if (answer == null) {
            throw new UnknownHostException(target.getHostName());
        }
        return response(answer);
    }

    /** Reads "HTTP/1.1 200 OK\r\nName: value\r\n...\r\n\r\nbody". */
    static HttpResponse response(byte[] answer) throws IOException {
        int end = -1;
        for (int i = 0; i + 3 < answer.length; i++) {
            if (answer[i] == '\r' && answer[i + 1] == '\n' && answer[i + 2] == '\r' && answer[i + 3] == '\n') {
                end = i;
                break;
            }
        }
        if (end < 0) {
            throw new IOException("offline answer without a header end");
        }
        String[] lines = new String(answer, 0, end, "ISO-8859-1").split("\r\n");
        String[] status = lines[0].split(" ", 3);
        BasicHttpResponse response = new BasicHttpResponse(new BasicStatusLine(HttpVersion.HTTP_1_1,
                Integer.parseInt(status[1]), status.length > 2 ? status[2] : ""));
        String type = null;
        for (int i = 1; i < lines.length; i++) {
            int colon = lines[i].indexOf(':');
            if (colon > 0) {
                String name = lines[i].substring(0, colon).trim();
                String value = lines[i].substring(colon + 1).trim();
                response.addHeader(name, value);
                if (name.equalsIgnoreCase("Content-Type")) {
                    type = value;
                }
            }
        }
        byte[] body = new byte[answer.length - end - 4];
        System.arraycopy(answer, end + 4, body, 0, body.length);
        ByteArrayEntity entity = new ByteArrayEntity(body);
        if (type != null) {
            entity.setContentType(type);
        }
        response.setEntity(entity);
        return response;
    }

    @Override
    public String toString() {
        return "OfflineDirector";
    }
}
