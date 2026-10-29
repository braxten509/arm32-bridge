package org.apache.http.impl.client;

import dev.braxten.gamebridge.OfflineDirector;
import org.apache.http.ConnectionReuseStrategy;
import org.apache.http.client.AuthenticationHandler;
import org.apache.http.client.HttpRequestRetryHandler;
import org.apache.http.client.RedirectHandler;
import org.apache.http.client.RequestDirector;
import org.apache.http.client.UserTokenHandler;
import org.apache.http.conn.ClientConnectionManager;
import org.apache.http.conn.ConnectionKeepAliveStrategy;
import org.apache.http.conn.routing.HttpRoutePlanner;
import org.apache.http.params.HttpParams;
import org.apache.http.protocol.HttpProcessor;
import org.apache.http.protocol.HttpRequestExecutor;

/**
 * Game Bridge's DefaultHttpClient for games that play offline. The converter renames the
 * game's DefaultHttpClient to this class in place (fix 7, converter/src/dex.rs: the name has
 * the same length), so every client the game makes is one of these. AbstractHttpClient's
 * execute methods are final and all end in the request director made here, which answers
 * from the phone instead of the network.
 */
public class OfflineHttpClient extends DefaultHttpClient {
    public OfflineHttpClient() {
        super();
    }

    public OfflineHttpClient(HttpParams params) {
        super(params);
    }

    public OfflineHttpClient(ClientConnectionManager manager, HttpParams params) {
        super(manager, params);
    }

    @Override
    protected RequestDirector createClientRequestDirector(HttpRequestExecutor executor, ClientConnectionManager manager,
            ConnectionReuseStrategy reuse, ConnectionKeepAliveStrategy keepAlive, HttpRoutePlanner routes,
            HttpProcessor processor, HttpRequestRetryHandler retries, RedirectHandler redirects,
            AuthenticationHandler targetAuth, AuthenticationHandler proxyAuth, UserTokenHandler users, HttpParams params) {
        return new OfflineDirector();
    }
}
